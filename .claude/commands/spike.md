---
name: spike
description: "Run a focused discovery spike: form hypothesis, design cheapest falsifying experiment, execute thin vertical slice, evaluate 2-3 approaches, and (when warranted) emit a CONTRACT-xxx task for foundational abstractions that will affect multiple downstream stories. Use when the riskiest assumption or multi-impact boundary is unclear before committing to a full PRD or large task list."
user_invocable: true
---

# /spike — Focused Experiment & Thin Slice

**Purpose**: Answer the single most important question for a risky or uncertain area with the least possible work, then decide the path forward (proceed with implementation, split, or define a stable `CONTRACT-xxx` for downstream work).

**When to use**:
- The riskiest assumption in the plan/PRD is still vague after a short interview.
- A design decision (abstraction, interface, data shape, error model, ownership boundary) will have ramifications on 2+ other stories.
- You want to collapse multiple "implement-and-rewrite" cycles into one informed cheap experiment.

**Usage**
```
/spike "risky area or multi-impact abstraction description"
/spike                          # Interactive
```

## Core Philosophy (Read This Every Time)

> **Canonical reference:** `~/.claude/docs/task-mgr-best-practices.md` — where this spike fits in the planning flow and how to wire `CONTRACT-xxx` / spawn-fixup tasks afterward.

> **CRITICAL — Spike output must be actionable and cheap to produce.**
>
> 1. **One hypothesis, one cheapest falsifier** — Name the riskiest assumption and the smallest experiment that could kill it (ideally < 2 hours of wall time).
> 2. **Thin vertical slice first** — Build the smallest end-to-end path that exercises the uncertain part. Do not build the full feature.
> 3. **2-3 approaches belong here** — This skill now owns the heavy "Approaches & Tradeoffs" and "Top-3 Risks + Inversion" work that used to live in every `/prd`.
> 4. **Emit CONTRACT-xxx when warranted** — If the spike reveals a foundational contract/abstraction that multiple later tasks will depend on, the primary output of the spike is a ready-to-use `CONTRACT-xxx` task (with extreme details, known-bad discriminators, and downstream impact list).
> 5. **Record the result** — Future PRDs and `/compound` will ask "Did we actually run the experiment we named?" — make the answer obvious in the progress log.

### Step 1: Clarify the Spike Target

Ask (at most 2-3 focused questions):

1. **What exactly is uncertain or high-impact?**
   - "The new abstraction for X that Tasks B, C, and D will all use."
   - "Whether approach A or B will survive real load on the first 5% of the data."

2. **What would a successful outcome look like after this spike?**
   - "A stable `CONTRACT-xxx` that the three downstream stories can implement against."
   - "A clear 'do not pursue' decision + the one rejected alternative documented."

3. **What is the absolute cheapest experiment that could falsify the current bet?**

Offer lettered options when helpful. Keep the scope tiny.

### Step 2: Explore Just Enough to Design the Experiment

Use Glob/Grep/Read only on the files that touch the uncertain boundary.

While exploring, actively look for:
- Existing patterns the thin slice should follow (or deliberately break).
- Callers / future consumers that the contract must satisfy.
- Data shapes that already cross the module boundary (trace key types exactly — this is the seed of the future Data Flow Contract).

**Do not** do a full codebase tour. Time-box to 15-20 minutes.

### Step 2.5: Recall Relevant Learnings (Fast)

```bash
task-mgr recall --query "<key concepts from the uncertain area>" --limit 8
task-mgr recall --tags "<domain>" --limit 5
```

Embed the 1-3 most relevant one-liners in your thinking and in the final output.

### Step 3: Run the Thin Experiment

1. State the hypothesis and the falsifying experiment in one paragraph (this paragraph goes into the progress log).
2. Implement the smallest possible vertical slice (often 1-2 files, minimal happy-path + the 1-2 hardest edge cases).
3. Run the scoped quality gate on just the touched files.
4. Measure what the experiment was supposed to tell you (latency, correctness on the named edge, coupling cost, clarity of the resulting contract, etc.).

If the slice reveals that a stable contract is the real deliverable, stop implementing and switch to writing the contract definition.

### Step 4: Evaluate Approaches & Decide

Now that you have data from the thin slice:

- Document 2-3 approaches (one of them is usually "what the slice just showed us").
- Use the same table format that used to live in `/prd` §6.
- Explicitly call out the Phase 2 Foundation implications (if any).
- Rank the top 2 risks that remain after the experiment.

**Decision output** (one of):
- A) Proceed with implementation using approach X — hand off to `/plan-tasks` or light `/tasks`.
- B) The uncertain part is now clear; the real artifact needed is a `CONTRACT-xxx` for downstream stories — emit the task.
- C) Kill the idea or radically change scope — document why.

### Step 5: Emit a CONTRACT-xxx (When Warranted)

Only when the spike concludes that a foundational abstraction will be used by 2+ other tasks:

1. Create a `CONTRACT-xxx` task via `task-mgr add --stdin` (or give the user the exact JSON to paste).
2. The task must contain:
   - Precise interface / data shape / error model / ownership.
   - All discovered edge cases + invariants.
   - Known-bad discriminators (what a wrong implementation of this contract would look like to consumers).
   - Failure modes.
   - Alternatives considered for the hard parts + rationale.
   - Explicit "Downstream Impact" list: the story IDs or future task names that will depend on this contract.
3. Set `taskType: "contract"`, low priority (0 or 1), `estimatedEffort: "medium"` or "high".
4. Record the full contract definition in the progress log so dependents can read it without re-exploring.

Example minimal `CONTRACT-xxx` JSON shape (adapt from the plan-tasks REVIEW template):

```json
{
  "id": "CONTRACT-001",
  "title": "Define stable AbstractionX contract for consumers B/C/D",
  "taskType": "contract",
  "description": "Design-only. Precise interface + extreme details so later FEATs can implement against it without drift.",
  "acceptanceCriteria": [
    "Interface (signatures + error types + ownership module) is written and unambiguous",
    "All edge cases from the spike + PRD are named with expected behavior",
    "Invariants that every implementation and every caller must maintain",
    "Known-bad: describe a plausible wrong implementation that would still pass naive tests",
    "Alternatives considered for the hardest case + one-line reason for rejection",
    "Downstream impact list: FEAT-003, FEAT-004, FEAT-007 (and any future stories) will dependOn this task",
    "Full contract text recorded in the progress log under ## CONTRACT-001"
  ],
  "priority": 1,
  "estimatedEffort": "medium",
  "touchesFiles": ["src/path/to/the/module/that/will/own/it.rs"],
  "dependsOn": [],
  "modifiesBehavior": false,
  "qualityDimensions": ["The contract must be stable enough that three independent implementations can be written against it without revision"],
  "notes": "Produced by /spike on <date>. Experiment result: ..."
}
```

Then tell the user: "Run `/plan-tasks` (or `/tasks`) next. The generated task list can now include `CONTRACT-001` as an early dependency for the affected FEATs."

### Step 6: Record the Spike Result (Mandatory)

Append one tight block to the current progress log (or the PRD's progress file if one exists):

```
## [YYYY-MM-DD] - SPIKE
Hypothesis: <one sentence>
Cheapest falsifier: <what we actually ran>
Result: <what the data showed — keep it factual>
Decision: <A / B / C from Step 4> + one-line rationale
CONTRACT emitted: CONTRACT-xxx (or none)
Key learning: <one sentence for future recall>
---
```

This is what `/compound` will later read to answer "Did we run the experiment we named?"

## Important Rules

- **Spikes are allowed to throw work away.** The goal is information, not shipping code.
- **Do not build the full feature.** If the thin slice turns into the full thing, you have scoped the spike too large.
- **Worktree discipline**: If you run `task-mgr add --stdin` or edit any task JSON, you are writing to the PRD's worktree. Use the same rules as `/review-loop` and `/compound` (resolve `WORKTREE`, `cd` into it, use absolute `$WORKTREE/...` paths for any Edit/Write, use `git -C $WORKTREE` for commits).
- **Context economy for the next agent**: Whatever you decide, the downstream prompt file must contain the distilled result (hypothesis + outcome + contract text) so the loop agent never has to re-read your spike notes.
- **One spike per uncertain area.** If you discover three separate risky abstractions, run three small spikes or split the work.

## After the Spike

**Recommended default hand-off** (most common case):

1. If you emitted a `CONTRACT-xxx`:
   - Run the exact `task-mgr add --stdin` command (or paste the JSON) so the contract task exists in the DB.
   - Immediately run the task generator for the PRD (`/plan-tasks` preferred for lean work, or light `/tasks`).
   - The generator will automatically see the early CONTRACT task and wire `dependsOn` edges correctly.

2. If no `CONTRACT-xxx` was needed:
   - Just say: "Ready for `/plan-tasks \"the original description\"` (or `/tasks` for a full PRD). The thin slice + chosen approach + experiment result are recorded above."

Do **not** tell the user to run bare `task-mgr init --from-json` at this point — use the canonical `task-mgr loop init ... --append --update-existing` only if you need to import an existing PRD JSON that doesn’t yet contain the CONTRACT task.

---

## Notes for Future Iterations of This Skill

- We may later add structured output (JSON) from `/spike` that can be fed directly into `/plan-tasks` or `/tasks`.
- Agent graduation path: if a particular domain produces many high-quality spikes that keep making the same class of discovery, that pattern can graduate into a specialist sub-agent (see `/compound` graduation logic).
- The "cheapest falsifying experiment" framing should be reused in the PRD and plan-mode templates so the language is consistent across the whole workflow.

This skill is intentionally lightweight. Its whole reason for existing is to let the rest of the process (PRD, tasks, loop) stay lean by moving the expensive uncertainty resolution earlier and making its output first-class (either a decision or a `CONTRACT-xxx`).

---

## Recommended New Default Flow (Post-These Changes)

For most work above ~4 tasks:

1. Short plan-mode interview or `/review-plan`.
2. If anything is risky or will define a contract used by 2+ later pieces → `/spike "..."`.
3. Spike outcome is either:
   - "Proceed — thin slice validated approach X" → run `/plan-tasks` (lean) or light `/tasks`.
   - "Need stable CONTRACT-xxx first" → the spike emits the `CONTRACT-xxx` task (via `task-mgr add` or JSON for the user). Then run the task generator; it will wire the early dependency.
4. Implementation proceeds with the lean skeleton (FEATs contain their tests, single final `REVIEW-001` is the milestone).
5. `/review-loop` + `/compound` now also audit spike fidelity and contract health.

Full heavyweight `/prd` is reserved for genuinely large, cross-subsystem efforts where the extra ceremony is justified.

Old PRDs using the previous 11-phase structure continue to work unchanged — the loop engine only cares about the individual tasks and the prompt file.