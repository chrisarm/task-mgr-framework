# PRD: Human Review Checkpoint

**Type**: Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-03-26
**Status**: Draft

---

## 1. Overview

### Problem Statement

The loop engine has no mechanism to pause for human input when a task completes. PRD authors can mark tasks with `"requiresHuman": true` (as seen in DeskMaiT's `prd-phase1-demo-loop.json` FEAT-004), but task-mgr silently ignores this field. The loop runs straight through milestone/checkpoint tasks that were intended as human review gates.

When the loop completes a `requiresHuman` task, it should pause interactively — show the task context, pose a prompt, wait for human input (with optional timeout), then inject that feedback into subsequent iterations so Claude adapts. Additionally, after collecting human input, the engine should run a Claude call to mutate downstream tasks (update descriptions, re-prioritize, add/remove tasks) based on the human's feedback before resuming.

### Background

- The `.pause` mechanism already provides the interactive infrastructure: file detection, stdin reading, `SessionGuidance` accumulation, and prompt injection. Human review checkpoints are conceptually an **auto-triggered pause** after task completion.
- Claude subprocess uses piped stdin (prompt written then closed), so the parent process retains terminal stdin between iterations — even in batch mode.
- The `update_prd_task_passes()` function in `prd_reconcile.rs` shows the pattern for atomically modifying PRD JSON mid-loop.
- `yes_mode` currently suppresses all interactive prompts; this must be overridden for `requiresHuman` tasks.

---

## 2. Goals

### Primary Goals

- [ ] Tasks with `requiresHuman: true` in PRD JSON trigger an interactive pause after completion
- [ ] The pause displays task context (ID, title, notes) and reads multi-line human input
- [ ] Human input is injected into subsequent prompts via `SessionGuidance`
- [ ] After human input, a Claude call mutates downstream PRD tasks based on the feedback
- [ ] Works in both single-loop and batch mode (overrides `yes_mode`)
- [ ] Optional per-task timeout auto-continues if no input is provided within the time limit

### Success Metrics

- `requiresHuman` tasks in existing DeskMaiT PRD (`FEAT-004`) trigger a pause when completed
- Human feedback appears in subsequent iteration prompts under `## Session Guidance`
- Downstream tasks in PRD JSON are updated after human input is processed
- Batch mode pauses for human review without requiring `--yes` to be removed

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Must detect task completion through ALL existing paths: `<completed>` tags, git commit detection, output scan, external repo reconciliation, and "already complete" fallback
- Must not block indefinitely in CI/headless environments — timeout or stdin EOF must gracefully continue
- Task mutation must be atomic (temp file + rename) to prevent PRD JSON corruption
- `requiresHuman` override of `yes_mode` must be scoped — only the review prompt ignores `yes_mode`, not other interactive prompts in the same iteration

### Performance Requirements

- DB query for `requires_human` is a primary key lookup on task completion — negligible cost
- Timeout implementation must not busy-wait; use `select!` or poll-based approach on stdin
- Claude task-mutation call should use a budget-conscious model (sonnet by default, configurable)

### Style Requirements

- Reuse existing `SessionGuidance` infrastructure — do not create a parallel accumulation system
- Follow the `handle_pause()` pattern for stdin interaction
- Follow the `update_prd_task_passes()` pattern for PRD JSON mutation
- New DB column follows existing migration patterns (v14 as template)

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| stdin EOF (piped/headless) | Batch runs in CI with no terminal | Treat as "no input", log warning, continue |
| Multiple tasks completed in one iteration | `<completed>` tags can mark multiple tasks done | Check each completed task for `requiresHuman`, pause for each that has it |
| Timeout fires mid-typing | User is typing when timeout expires | Collect whatever has been typed so far, use it as input |
| Task completed via external repo reconciliation | External git scan marks task done post-iteration | Still triggers human review (checked after all completion paths) |
| PRD JSON mutation fails | Claude produces invalid JSON or conflicting edits | Log error, continue with session guidance only (graceful degradation) |
| `requiresHuman` on a task that was already done at import | `passes: true` + `requiresHuman: true` | Skip review — task was pre-completed, no review needed |

---

## 3. User Stories

### US-001: PRD Author Marks Task for Human Review

**As a** PRD author
**I want** to add `"requiresHuman": true` to a task in my PRD JSON
**So that** the loop pauses after completing that task and waits for my feedback

**Acceptance Criteria:**

- [ ] `requiresHuman` field is parsed from PRD JSON and stored in DB
- [ ] Field is optional, defaults to false when absent
- [ ] `task-mgr show <task-id>` displays "Requires Human Review: Yes" when set
- [ ] `task-mgr next` output includes the field when present

### US-002: Loop Pauses for Human Review After Task Completion

**As a** developer running a loop
**I want** the loop to pause interactively when a `requiresHuman` task completes
**So that** I can review the work and provide feedback before subsequent tasks begin

**Acceptance Criteria:**

- [ ] Banner displays task ID, title, and notes (as the review prompt)
- [ ] Multi-line stdin input accepted (empty line to continue)
- [ ] Input stored as session guidance tagged with task ID
- [ ] Guidance injected into subsequent prompts via `## Session Guidance`
- [ ] Empty input (just Enter) continues without recording guidance

### US-003: Optional Timeout for Human Review

**As a** PRD author
**I want** to set an optional timeout on human review tasks
**So that** the loop auto-continues if I don't respond within the time limit

**Acceptance Criteria:**

- [ ] `humanReviewTimeout` field in PRD JSON (integer, seconds)
- [ ] Countdown displayed during wait (e.g., "Waiting for input... 45s remaining")
- [ ] On timeout: log that review was skipped, continue loop
- [ ] Partial input collected before timeout is used as feedback

### US-004: Task Mutation After Human Input

**As a** developer
**I want** Claude to update downstream tasks based on my review feedback
**So that** subsequent tasks reflect my decisions without manual PRD editing

**Acceptance Criteria:**

- [ ] After human input, a Claude call processes the feedback against remaining tasks
- [ ] Claude can modify task descriptions, notes, acceptance criteria, and priorities
- [ ] Claude can add new tasks or mark existing tasks as irrelevant
- [ ] PRD JSON is updated atomically (temp file + rename)
- [ ] DB is re-synced from updated PRD JSON
- [ ] If mutation fails, loop continues with session guidance only (no data loss)

### US-005: Batch Mode Supports Human Review

**As a** developer running batch/chain mode
**I want** `requiresHuman` tasks to pause for input even in batch mode
**So that** I don't have to choose between batch convenience and review gates

**Acceptance Criteria:**

- [ ] `requiresHuman` overrides `yes_mode` for the review prompt only
- [ ] Other `yes_mode` behaviors (worktree cleanup, key decision deferral) remain auto-confirmed
- [ ] stdin is available between iterations in batch mode (already true — Claude subprocess uses piped stdin)

---

## 4. Functional Requirements

### FR-001: `requiresHuman` Field in PRD JSON and DB

Add `requiresHuman: bool` (default false) and `humanReviewTimeout: Option<u32>` (seconds) to the PRD task schema. Store as `requires_human INTEGER DEFAULT 0` and `human_review_timeout INTEGER DEFAULT NULL` in the tasks table.

**Details:**

- Parse from `PrdUserStory` struct (camelCase via serde)
- Store in tasks table via migration v15
- Include in INSERT and UPDATE SQL in import.rs
- Include in Task model with `TryFrom<&Row>` support
- Display in `show` and `next` command output
- Include in export for round-tripping

### FR-002: Human Review Handler

Create `handle_human_review()` in `signals.rs` that reuses the `.pause` stdin-reading pattern but with task-specific context.

**Details:**

- Display banner with task ID, title, and notes (notes serve as the review prompt)
- Read multi-line stdin until empty line (identical to `handle_pause`)
- Tag guidance as `[Human Review for {task_id}] {input}` in `SessionGuidance`
- Support optional timeout via non-blocking stdin read
- In headless environments (stdin EOF), log warning and continue

### FR-003: Loop Engine Integration

Trigger human review after task completion detection in `engine.rs`, after all completion paths have converged (line ~1811), before iteration counter tracking (line ~1813).

**Details:**

- Collect all task IDs completed this iteration
- For each, query `requires_human` from DB
- If true, call `handle_human_review()` with task context
- `requiresHuman` overrides `yes_mode` — the review prompt always appears if the task has the flag
- Skip review for tasks that were `passes: true` at import time (pre-completed)

### FR-004: Task Mutation via Claude Call

After collecting human input for a review checkpoint, spawn a Claude call to process the feedback and update downstream tasks.

**Details:**

- Build a focused prompt: human feedback + current PRD state (remaining todo tasks) + instruction to update tasks
- Spawn Claude subprocess with the mutation prompt
- Parse Claude's output for PRD JSON modifications
- Apply modifications atomically to PRD JSON file
- Re-sync DB by calling the existing init/import flow for changed tasks
- Use budget-conscious model (sonnet default, configurable via `humanReviewModel` on PRD)
- On any failure, log error and fall through to session-guidance-only path

### FR-005: Timeout Support

Implement optional timeout for human review prompts using non-blocking stdin.

**Details:**

- Use `poll`-based or `select`-based approach to check stdin readiness with a deadline
- Display countdown (update every 10s: "Waiting for input... 45s remaining")
- On timeout: collect any partial input, log "Human review timed out after Ns", continue
- Zero or absent timeout = wait indefinitely (current `.pause` behavior)

---

## 5. Non-Goals (Out of Scope)

- **Interactive task editing UI** — human provides free-text feedback, Claude interprets it. No structured form/menu.
- **Review approval/rejection workflow** — this is a feedback injection mechanism, not a gating system. The loop always continues after the pause.
- **Persistent review history** — reviews are captured in session guidance and progress.txt, not a dedicated review table.
- **Auto-detection of review-worthy tasks** — only explicit `requiresHuman: true` triggers a pause. No heuristic detection based on task ID patterns like "MILESTONE".

---

## 6. Technical Considerations

### Affected Components

| File | Change |
| --- | --- |
| `src/db/migrations/v15.rs` | **New** — migration adding `requires_human` and `human_review_timeout` columns |
| `src/db/migrations/mod.rs` | Register v15, bump `CURRENT_SCHEMA_VERSION` to 15 |
| `src/commands/init/parse.rs` | Add `requires_human` and `human_review_timeout` to `PrdUserStory` |
| `src/models/task.rs` | Add fields to `Task` struct + `TryFrom<&Row>` |
| `src/commands/init/import.rs` | Bind fields in `insert_task()` and `update_task()` SQL |
| `src/loop_engine/signals.rs` | New `handle_human_review()` function |
| `src/loop_engine/engine.rs` | Trigger review after completion detection; call task mutation |
| `src/loop_engine/prd_reconcile.rs` | New `mutate_prd_tasks()` function for Claude-driven task updates |
| `src/loop_engine/claude.rs` | Possibly extend to support the mutation call (or reuse existing `run_claude`) |
| `src/commands/show.rs` | Display `requires_human` and timeout in show output |
| `src/commands/next/output.rs` | Include field in next command output |
| `src/commands/export/prd.rs` | Export field for round-tripping |

### Dependencies

- No new external crates needed
- Timeout implementation can use `std::io` with `poll`/`select` via `libc` or the existing `mio` if available; alternatively `std::thread::spawn` + channel with timeout

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A: Auto-triggered pause (reuse SessionGuidance)** | Minimal code; reuses existing `.pause` infrastructure; guidance flows naturally into prompts | No task mutation — Claude adapts via prompt context only | Rejected — user explicitly wants task mutation |
| **B: Auto-pause + Claude mutation call** | Full feature; human feedback updates actual PRD tasks; subsequent iterations work from corrected task definitions | More complex; needs a second Claude call per review; mutation can fail | **Preferred** — matches user requirement; graceful degradation to approach A on failure |
| **C: Webhook/external notification** | Works in fully headless CI; could integrate with Slack/email | Requires external infra; async response model doesn't fit synchronous loop | Rejected — over-engineered for current needs |

**Selected Approach**: B — Auto-pause with Claude task mutation. On mutation failure, gracefully degrade to A (session guidance only).

**Phase 2 Foundation Check**: This approach lays a strong foundation. The `requiresHuman` field and mutation infrastructure can later support: approval gates (task doesn't complete until human approves), structured feedback forms, and CI webhook integration. Cost now is ~2 days; avoiding rework of a simpler approach later saves ~1-2 weeks. Meets the 1:10 ratio threshold.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Claude mutation produces invalid PRD JSON | Medium — corrupted PRD file | Medium | Atomic write (temp + rename); validate JSON before write; keep backup of pre-mutation file |
| Timeout implementation is platform-dependent | Low — only affects timeout feature | Low | Use `std::thread` + channel approach (portable); fall back to blocking read if thread spawn fails |
| `requiresHuman` overriding `yes_mode` surprises batch users | Medium — unexpected pause in automated pipeline | Low | Log a clear message at loop start: "N tasks require human review — loop will pause for input" |

### Security Considerations

- Human input is injected into Claude prompts — no special sanitization needed (Claude handles arbitrary text)
- PRD JSON mutation is constrained to the PRD file path already known to the engine — no path traversal risk
- No secrets or credentials involved in the review flow

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `signals::handle_human_review` | `(task_id: &str, task_title: &str, task_notes: &str, iteration: u32, session_guidance: &mut SessionGuidance, timeout_secs: Option<u32>) -> bool` | `true` if input was provided | N/A (infallible) | Reads stdin, mutates `session_guidance` |
| `prd_reconcile::mutate_prd_tasks` | `(prd_path: &Path, human_feedback: &str, task_prefix: Option<&str>, model: Option<&str>) -> TaskMgrResult<MutationResult>` | `MutationResult { tasks_modified, tasks_added, tasks_removed }` | `TaskMgrError` | Spawns Claude, writes PRD JSON, updates DB |

#### Modified Interfaces

| Module/Function | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `PrdUserStory` (parse.rs) | (no `requires_human` field) | `+ pub requires_human: Option<bool>` | No | serde default = None, ignored if absent |
| `Task` (task.rs) | (no `requires_human` field) | `+ pub requires_human: bool` | No | DB default = 0, model default = false |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Access Pattern |
| --- | --- | --- |
| PRD JSON `requiresHuman` -> DB `requires_human` -> engine review trigger | JSON bool (`requiresHuman`) -> `PrdUserStory.requires_human: Option<bool>` -> DB `INTEGER DEFAULT 0` -> `Task.requires_human: bool` | `story.requires_human.unwrap_or(false)` at import; `task.requires_human` at engine check |
| Human input -> SessionGuidance -> prompt section | `String` (stdin) -> `GuidanceEntry { iteration, text }` -> `format_for_prompt()` -> `## Session Guidance` in prompt | `session_guidance.add(iteration, format!("[Human Review for {}] {}", task_id, input))` |

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `docs/ARCHITECTURE.md` | Update | Add Human Review Checkpoint to Task State Machine and Loop Iteration Flow sections |
| `CLAUDE.md` | Update | Add note about `requiresHuman` field for PRD authors |

---

## 7. Open Questions

- [ ] **Mutation model**: Should the default model for the task-mutation Claude call be configurable per-PRD (`humanReviewModel`) or use a global default (sonnet)?
- [ ] **Mutation scope**: Should Claude be allowed to modify ALL remaining tasks, or only tasks that depend on the reviewed task (safer, more constrained)?
- [ ] **Review on failure**: Should `requiresHuman` also trigger if the task fails/blocks (not just completes)? Could be useful for "human, help me debug this" scenarios.

---

## Appendix

### Real-World Example

From DeskMaiT `prd-phase1-demo-loop.json`, FEAT-004:

```json
{
  "id": "FEAT-004",
  "title": "Golden component: StatusPill + shared components",
  "requiresHuman": true,
  "notes": "HUMAN CHECKPOINT: After building all components, run `cd harmony-console && npm run dev` and visually inspect StatusPill..."
}
```

Expected behavior after this feature:
1. Loop completes FEAT-004
2. Engine detects `requires_human = true`
3. Displays banner with task title + notes as review prompt
4. Human reviews visual output, types feedback: "StatusPill looks great, but HealthDot needs darker green for healthy state"
5. Claude mutation call updates FEAT-005, FEAT-006 etc. to reflect the color correction
6. DB re-synced, loop continues with updated task definitions + session guidance

### Glossary

- **`requiresHuman`**: Boolean field on PRD tasks that triggers an interactive pause after task completion
- **Task mutation**: A Claude call that modifies downstream PRD tasks based on human review feedback
- **Session guidance**: Accumulated human input from `.pause` and review interactions, injected into subsequent prompts
