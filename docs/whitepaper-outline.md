# Whitepaper Outline: How to Keep a Coding Agent Working While You Sleep

**Subtitle:** *A practical methodology for long-running coding agents — so the work still points the right way at 3 a.m.*

**Abstract (draft this before the sections):** Coding agents finish a focused chat and lose the plot on multi-hour jobs: they forget decisions, declare done early, and cannot say what is left. The human designs the system — goals, small checkable tasks, scheduled human gates, and memory outside the chat — and the agent runs one task per fresh session against that memory. A verification skill drives the real app, so a green unit suite is not the only proof. Sanding that track is slower than pulling the train by hand; once the bumps are written down, the run completes more good laps than hand supervision can. Each piece of ceremony assumes a failure the current model still has. When the model changes, re-check the assumption and drop the piece that no longer earns its cost.

**Audience:** People who already use AI for coding (Claude Code, Grok Build, Codex, Cursor, etc.) and can get small jobs done in a 10–15 minute chat — but lose the plot when the job is big: multi-file features, multi-day refactors, “finish this while I’m offline.” This paper is for that second class of work.

**What this is not:** A model comparison, a prompt catalog, or a general “AI for everything” workflow. It is a software-engineering methodology for **bounded autonomy** (supervised long runs), not full autopilot.

**When *not* to use this (preview — full decision table in §17):** If the job fits in one focused conversation and you can watch it, just chat. The ceremony below pays off when the work is large enough that “done” is ambiguous, steps depend on each other, or you will not be at the keyboard for every decision. A pile of repetitive spec files on a small change is the failure mode reviewers of spec-driven kits already complain about; §17 is the answer.

---

## Design notes for drafting (not part of the published paper)

- **One canonical story:** “Add email reminders to the notes app.” Earlier sections carry one snippet each; §15 assembles them once.
- **One metaphor:** the wooden train track (§1.5), told in full once. Later sections only point back (§12, §16.7, §19). Do not retell it.
- **One home per fact:** the §7.5 table is binding. Do not restate a plan rule inside the journal, or a learning inside the contract.
- **Callout types:** **Rule** · **Anti-pattern** · **Field note** (real systems, anonymized). Public write-ups are cited by short name + date, with the URL in Appendix J.
- **Principle vs tool:** Body states principles. Product names (Ralph, Beads, Kiro, a given CLI) appear in field notes and Appendix I, never as “you must install this.”
- **Numbers date.** Size limits, try budgets, dollar figures, and METR horizons are starting observations with a date. The principle is the failure they guard against, plus the re-audit rule (§17.6).
- **Plan = what/why; tasks = how/checkable.** Spike owns the approach comparison; the plan keeps the verdict and one rejection.
- **Externalized memory over chat history.** A fresh session is a **context reset** with a handoff, chosen over compaction (§16.2).
- **Bolded terms** all appear in Appendix A.
- **Length:** one primary example and one failure mode per major section. Cut from the body (do not resurrect): café-search edge, user-accounts split walkthrough, mobile cross-project vignette, silent-nested-options, search-library bake-off, benefits-stacking composite.
- **Figure after §1:** spike → plan → interview → contracts → tasks → graph → session start → loop → code gates → user-path proof → memory. Inset: hand-pulled laps vs self-propelled laps after the bumps are sanded.
- **Series, then the rollup.** Six standalone articles, then this outline as the whitepaper. Satellites hold procedure, field notes, and dated evidence. The whitepaper holds what only works together: abstract, the train story in full, the §7.5 fact-home table, the overnight contract (§9.7), the decision table and re-audit rule (§17), one compressed reminders walkthrough (§15), the three non-negotiables (§19.2), and the four measurements (§18). Elsewhere the train is one sentence and a link. A term is defined in the article that owns it and used, not redefined, afterward. About 1,500–2,500 words per article; about 4,000–6,000 for the whitepaper.

  | Order | Article | Owns these sections | Whitepaper keeps |
  |---|---|---|---|
  | 1 | **The loop** | §9, §10, §11, §13.1–§13.5 | You can sleep only when the overnight contract is true |
  | 2 | **Context** | §4.4, §7, §16.2 | A fresh session is a reset with a handoff; the reload pays only while the sheet stays small |
  | 3 | **Graphs** | §6.8, §8, §9.5 | One picture gives order and safe parallelism; file facts go stale |
  | 4 | **Proving the user path** | §6.3–§6.5, §12 | A green unit suite is not a lap; a passing drive is evidence for the path you drove |
  | 5 | **Memory** | §3.5, §6.11, §14 | A lesson after task 5 is on the sheet for task 6, and still there for the next project |
  | 6 | **Writing the track** | §2, §3, §4, §5, §6 | The plan says what and why; tasks say how and how you check |
  | 7 | **Whitepaper** | §1, §15–§19, plus the kept boxes above | The argument that joins the six |

  Publish in that order. Week-1 adoption (§17.3) links to Writing the track and to the loop’s start ritual; it does not wait for all six. Routing, human gates, the failure ladder, and the lethal trifecta stay inside the loop article. The trifecta is repeated as a box on the overnight contract so a rollup-only reader still sees it. Each article gets one claim, one failure mode, and one beat of the reminders story. The whitepaper is the only full pass.

---

## 1. Introduction: Why Coding Agents Need a Plan

- 1.1 What a **coding agent** is: a helper that can read a repo, edit files, run tests, and change systems with little hand-holding in the moment.
- 1.2 The core problem: agents are strong on small jobs and get lost on big ones. They forget earlier decisions, invent scope, declare “done” early, and have no honest answer to “what’s left?” The same early-victory failures show up in public long-running harnesses (Anthropic, Nov 2025): the agent declares the app done, or marks a feature done, before the list agrees.
- 1.3 The big idea: don’t hand the agent one giant job. Give it a **workflow** — a written plan of small tasks, with rules, checkpoints, right-sized models, human stop points, and a **memory** outside the chat.
- 1.4 **Bounded autonomy:** the human designs the *system* (goals, checks, when to wake a person); the agent runs the *steps*. Oversight is scheduled and machine-readable, which is what makes sleep possible.
  - 1.4.1 **Why this matters now (dated).** METR’s time-horizon reports (Mar 2025; methodology update Jan 2026) measure how long a task, in skilled-human time, an agent finishes with about 50% reliability. That length has been doubling on a timescale of months, so multi-hour jobs are now inside what agents will *attempt*. Fifty percent at that horizon is the case for a designed system over full autopilot. Re-check the figure when drafting; the principle survives if the constant moves (§17.6).
- 1.5 **The wooden track.** Told once, below. Hand-pulled = sitting in the chat and fixing each derailment as it happens. Self-propelled = the agent running the task list. Sanding the track = writing down every bump, turn, and grade: plan, contracts, gates, and a **verification skill** that drives the real app. Throughput is good laps completed without you in the loop.
- 1.6 Who this is for: engineers and tech leads who already pair with coding agents and want multi-hour or overnight runs on features, migrations, and cleanups. Not a general-agent essay.
- 1.7 Where this sits (five lines; the map is Appendix I). A design doc records intent. A ticket tracks status for people. A free-roaming agent has neither a durable leftover list nor a handoff. This method is a **spec-anchored** plan (the spec guides the work and stays editable) plus an executable task graph, memory outside the chat, and a bounded loop. It does not claim to have invented planning.
- 1.8 What you will learn, and what this paper will not do: the cycle, how plans differ from tasks, how a user-path proof raises lap count, and when to skip or retire a piece. Not vendor rankings, magic prompts, or a replacement for human merge review (§12.5).
- 1.9 Map of the paper (figure).

> **Story — the wooden track:** You have an old toy train on a wooden track, the kind that drives itself. The first time you set it down, it derails constantly. A bump in a joint, a turn that is too sharp, a short rise the wheels cannot climb. Finding those spots feels slow. It is slow, next to the other way you already know: hold the train in your hand and pull it around the loop, setting it back on the rails every time it jumps. Hand-pulled, you finish a few laps and you have something to show. You also did every inch yourself. The self-propelled engine only pulls ahead after the track is actually smooth — each bump sanded, each turn eased, each grade made climbable, and written down so the next lap does not rediscover it. Then you stop touching it. It keeps going, and it starts passing the cars you were dragging by hand. You will never make as many laps manually as that engine makes once the track is ironed out. The rest of this paper is how you find the bumps before you go to sleep, including the ones unit tests cannot see.

> **Field note — the billing feature that “done!”’d too early:** A team asks an agent to “build our whole billing feature.” It works for an hour, says done — half the screens missing, tests red, no leftover list. The same team then breaks the job into ~20 small tasks with checklists. When the agent stops, the list shows what is done, what failed, and what is left.

> **Anti-pattern:** The full methodology on a 15-minute typo fix. That is a conversation.

---

## 2. Start With Discovery: Look Before You Leap

- 2.1 **Exploration spike:** one small, cheap experiment aimed at the **riskiest guess**.
  - 2.1.1 Write the guess as a **hypothesis** that could be proven wrong.
  - 2.1.2 Build the thinnest version that can fail.
  - 2.1.3 Compare 2–3 approaches and pick one. This comparison is *not* repeated in the plan. The plan keeps the verdict and at least one rejected option with its reason (§3.14).
- 2.2 A wrong answer on day one is cheap. The same wrong answer on day twenty is the rework the spike exists to avoid.
- 2.3 **Timebox, and write the fallback first.** Every open investigation has a deadline and a pre-chosen default (“if the bake-off passes three days, ship library X and record why”).
- 2.4 **Anti-pattern — research theater:** exploration with no decision artifact. A spike emits a decision, and sometimes a contract seed (§5).
- 2.5 Output: a short learning note plus that optional seed.

> **Field note — map the real API (canonical story starts here):** Before designing email reminders, spend one afternoon calling the mail provider. Save real responses for success, bad request, auth failure, and outage. Later failure tasks (§13) name those codes.

---

## 3. Project Planning: Writing Down What “Done” Means

> **Rule — plan says *what* and *why*, not *how*.** The plan is the answer sheet for an agent alone at 3 a.m. Files, proof commands, and trap tests belong in tasks (§6).

### 3.A What kind of job, and how big is the plan?

- 3.1 **Name the job kind.** Feature, bug fix, enhancement, or cleanup (no behavior change). Bugs need a reproduction and expected-vs-actual. Features need who it is for and the smallest useful version.
- 3.2 **Match plan size to project size.** A paragraph for small multi-file work; a full document for multi-day work. A single chat (§17) skips this section. This is the direct answer to “I’d rather review the code than a stack of spec files.”
- 3.3 The first draft has holes on purpose. The interview (§4) exists to find them.

### 3.B Look around before you write

- 3.4 Read the existing system: likely files, patterns worth copying, edge cases the code already handles, designs already present.
- 3.5 Check **team memory** (§14) by topic and by keyword. Paste the lessons that change this plan into the plan, so the worker does not depend on remembering to search.

### 3.C What a good plan contains

- 3.6 **Goal** — problem and for whom — plus **success measures** (numbers). **Quality dimensions** (correctness, performance, house patterns) say how good the solution must be while it ships; measures say the outcome.
- 3.7 **User stories** — “As a *[role]*, I want *[ability]*, so that *[benefit]*.”
- 3.8 **Invariant seeds** — one line each for what must stay true. §5 specifies the invariant and names its owner. The plan does not restate the contract.
- 3.9 **Named edge cases** — concrete inputs with expected behavior. Named cases get handled. Unnamed ones reach customers.
- 3.10 **Failure edges** — bad input, missing files, network down, at plan level. The response ladder is §13.
- 3.11 **Out of scope**, on purpose.
- 3.12 **Deliberate cuts** — the one or two vision pieces that cost the most and help the least, cut or deferred in writing before task design.

> **Canonical story — plan core:** *Goal* — a reminder 24h before a due date. *Measure* — the send attempt finishes under N seconds. *Story* — a note-taker who does not want to miss deadlines. *Invariant seed* — never send the same reminder twice (§5 specifies it). *Edge* — provider down means retry with backoff, and the user’s intent is not dropped. *Out of scope* — SMS. *Cut* — custom reminder sounds. *Stop-sign* — the provider’s daily cap (§3.16).

### 3.D Verdict, then stop

- 3.13 The plan records the spike’s verdict: choice, why, and one *rejected* option so nobody re-argues it. Combining two approaches is fine if the plan says so. Prefer a foundation when rework would compound.
- 3.14 Do not run a second 2–3-way bake-off here.

### 3.E Hardening (short here; checklist in Appendix D)

- 3.15 **Consumers of a behavior change.** List callers. Mark fine, breaks, or needs a look. The same function serving two purposes splits into two pieces of work.
- 3.16 **Top 3 risks** by inversion: “How would this design guarantee failure?” Rank damage × likelihood. **Stop-sign rule:** big and likely means a human decides before task design.
- 3.17 **Completeness check** (Appendix D): quality filled, at least two named edges, at least one rejected approach, risks, consumers, cuts, docs plan or “none — on purpose.” A blank is a forget, not a choice.

> **Field note — same function, two purposes:** A date-format “fix” breaks file-naming callers that cannot accept the new character. The plan splits UI format from filename format.

> **Canonical story — stop-sign:** The provider allows 100 emails/day, and the user count makes that likely. A human picks a plan upgrade or batching before tasks are written.

---

## 4. The Interview: Let the Agent Ask Before You Design Tasks

- 4.1 Missing requirements are questions nobody asked.
- 4.2 **Plan interview:** an agent whose only job is clarifying questions.
  - 4.2.1 Probe fuzzy words, unstated limits, conflicting rules, hard-to-undo choices, unnamed edges, missing quality targets, and scope that should be cut.
  - 4.2.2 Three to five questions a round.
  - 4.2.3 Multiple-choice answers, plus a suggested default.
  - 4.2.4 The human answers, or the question becomes a human-decision task (§11).
- 4.3 **Role separation:** interviewer and worker are different sessions. The worker sees the updated plan, not the wandering.
- 4.4 **Forked conversation.** Exploration can spend a long context. What comes back is a short decision list (on the order of a 1–2k-token handoff, not the transcript). Public context guidance is the same idea: a sub-agent may explore widely and should return a summary (Anthropic, context engineering).
  - 4.4.1 **Rule:** the conversation is disposable; the **decisions** are not.
- 4.5 **Write decisions back into the plan** before task design. **Rule — if it isn’t in the plan, it didn’t happen.**
- 4.6 Stop when another round does not change the plan. Two or three rounds is typical. Ten rounds means you planned before the spike, or the scope is still too big.
- 4.7 **Limit:** interviewer and worker from the same model family share blind spots. A different session reduces transcript contamination. It does not create independent evidence. Trap tests, the blocked exit, and a stronger reviewer (§12.5) are what cover that gap.

> **Canonical story — interview:** “Remind 24 hours before” raises time zone, a due date edited after queue, and shared vs private notes. Answers go into the plan. Format tangents stay in the fork.

> **Anti-pattern — interview without write-back:** The Q&A was good. The plan never changed. A later worker builds the only privacy behavior it can see.

---

## 5. Boundary Contracts: Agree on the Connectors First

- 5.1 A **contract** is a promise about how two pieces of work fit: data shape, names, rules.
- 5.2 If several tasks plug into one connector, design the connector first or they will guess several shapes.
- 5.3 A connector that **two or more** downstream tasks build against gets its own contract task. Dependents point at it (§8). A single caller does not get a contract task. Changing the contract reopens it and reopens the tasks that build against it.
- 5.4 **Invariants** are the §3.8 seeds, now specified and owned. “Never double-send” becomes a check, not a slogan. An optional **property-based test** states it for a whole input class (“for any due-date edit, at most one reminder is sent”). That sits beside example tests; it does not replace them (§6.5).
- 5.5 Both sides: inputs including bad input, success and failure outputs, side effects, and a migration note if current callers exist.
- 5.6 **Show a verified, copy-paste example** traced from the real system. A wrong shape often looks right, runs clean, and returns empty.
- 5.7 Keep new connectors few. Each concept has one owner module.
- 5.8 **Trio:** contract = shared interface; invariant = always-true rule; **acceptance criteria** = this task’s yes/no checks (§6). Public harnesses sometimes call the per-chunk yes/no a “sprint contract” (Anthropic, Mar 2026). Use acceptance criteria in this paper so the glossary stays one term.

> **Canonical story — contract:** One reminder record (note ID, send time, sent flag) with a verified read, before send, UI, or import tasks start. The double-send invariant has an owner.

> **Field note — five shapes of “task record”:** List, picker, progress report, importer, and reviewer each invent a record. Merge is the bill for skipping the contract.

---

## 6. Task Design: Breaking the Plan Into Bites

- 6.1 A good task is one sitting, clear to a stranger, and provable. **One task per loop round.**
- 6.2 **Size bounds are starting observations, not laws of nature.** From one production workflow, tasks that passed about four code-changing checklist items, about four files, or about 150 words of description started to thrash; past about twelve items or about ten files they needed a hard split. Treat those as the current sanding marks. Split when a task bounces or the agent loses the thread. Re-measure when the model changes (§17.6). Difficulty comes from the same signals: one tiny edit is easy; a few files plus a new function and tests is medium; cross-cutting work is hard and is a hint to split.
- 6.3 **Acceptance criteria** are yes/no. No “cleaner.” Settled before code, which is the whole point of writing them into the task rather than discovering “done” at the end.
  - 6.3.1 **Global acceptance criteria**, stated once: automated checks pass; no new warnings; nothing existing breaks, under the gate policy in §12.
  - 6.3.2 **Prohibited outcomes**, also on the standing sheet (§7), because wording changes behavior: tests that only check “didn’t crash”; errors with no cause; swallowed errors; single-use abstractions. Add the **reward-hacking** moves seen when tests contradict the spec (ImpossibleBench, Oct 2025): editing the tests, making equality always return true, keeping hidden state so a check passes, and special-casing the exact test input. Read-only tests block the first move and not the others, so the trap test stays.
- 6.4 Copy the quality dimensions that apply onto the task, so task 14 does not re-read the plan.
- 6.5 **Every named edge becomes a test.**
  - 6.5.1 **Trap test:** at least one test a lazy, plausible-wrong solution fails. The double-send fake that always returns “ok” is the canonical one.
  - 6.5.2 Fixtures use the real record shape (§5.6).
  - 6.5.3 When an invariant is a “for any input” claim, add a property-based check. A handful of generated cases is enough to start. This is evidence, not a proof of all inputs.
- 6.6 Every task carries: collision-resistant ID (parallel workers will spawn fix-ups), title, difficulty, **files touched**, hard dependencies, and optional pre-check, completion check, time budget, notes, needs-human. No model name (§6.10).
- 6.7 **Foundations first:** shapes and types, then core logic, then integrations, then UI.
- 6.8 **Record facts; derive relationships.** Files touched are facts. “Cannot run in parallel” is derived. Only “B needs A’s output” is written as a hard edge. Hand-maintained conflict lists drift. File facts themselves go stale as the code moves; parallelism is only as safe as the last update (§17.5).
- 6.9 Task types:
  - 6.9.1 **Build** — one story and its tests.
  - 6.9.2 **Contract** — design only. Interface, edges, failures, the wrong implementation that would look right, dependents. No production code. A change reopens this task and its dependents.
  - 6.9.3 **Review** — look, don’t fix. Judge against the plan, spawn fix-ups, never implement inline.
  - 6.9.4 **Fix-up** — root cause, exact fix, proof command, so a cheaper model can land it in one pass.
  - 6.9.5 **Human-decision** — §11.
- 6.10 **Tasks say how hard, and what role they are.** Difficulty and task type. The routing table (§10) maps difficulty × role to a capability. Model IDs live only in that table.
- 6.11 Recall memory again at task-design time, with file and function names, into notes or acceptance criteria.
- 6.12 Validate the list (Appendix E) before the first run.

> **Example — bad vs good task:** Bad: “Improve the list command.” Good: “TASK-012: Add `--status` filter. Difficulty easy. Touches list + tests. Depends on the record contract. AC: filter works, unknown status errors clearly, default unchanged, tests cover all three.”

> **Canonical story — trap test:** “A reminder sent twice is reported, not absorbed.” The always-ok fake fails.

> **Example — fix-up anatomy:** Root cause: line 84 skips with no message. Exact fix: collect and report line numbers. Proof: the bad-rows fixture lists lines 3 and 7.

---

## 7. The Standing Instruction Sheet and Context Thrift

> This artifact is as important as the task list. It is the handoff a fresh session actually reads.

- 7.1 One short document, re-read every round, holding what applies to *every* task.
- 7.2 Contents:
  - 7.2.1 **Priority when goals collide:** anticipate edges → foundations → working code → provable correctness → clean code → polish.
  - 7.2.2 Global acceptance criteria, prohibited outcomes (§6.3), distilled contracts, a pointer to the verification skill’s feature file (§12), and 5–10 one-line lessons.
  - 7.2.3 **Pointers, not pastes.** “Plan §edges,” “drive feature *reminders*.” Load the section when the task names it. Public context guidance calls this just-in-time retrieval.
- 7.3 **Context rot** is the mechanism: as the window fills, recall of what is already in it gets worse (Anthropic, context engineering). The sheet exists so the round does not begin already full.
- 7.4 **Two different files.** A repo-level `AGENTS.md` (the Linux Foundation’s Agentic AI Foundation standard; the closest file to the edit wins) holds stable rules for every project in the repo. The **standing instruction sheet** holds *this* run: this plan’s contracts, this run’s lessons, this feature pointer. Readers will ask. Answer in one sentence, then do not mix the files.
- 7.5 **Where a fact lives.** One home. Later sections must not invent a second.

  | Fact | Home |
  |---|---|
  | Intent, cuts, rejected approach | Plan |
  | Shared interface and verified example | Contract |
  | This step’s proof | Task acceptance criteria |
  | What this run actually did | Progress journal |
  | A scar worth recalling next project | Learning |
  | An option rejected so it is not relitigated | Decision log |
  | How to start and drive the real app | Verification skill / feature map |

- 7.6 **Anti-pattern:** every round re-reads a 900-line plan, the whole guidebook, and the whole log. Quality collapses before any code is written.

> **Field note — thrift:** With the sheet, a round starts from a few pages: rules, contracts, ten lessons, the current task. Without it, the window is gone on history.

---

## 8. Ordering the Work: The Dependency Graph

- 8.1 A **DAG**: an arrow means “must finish first.” **Acyclic** means no loops.
- 8.2 Check the plan with the graph:
  - 8.2.1 A cycle is a deadlock. Detect it with a tool before the run, not with a reminder in the prompt (§13.4).
  - 8.2.2 An orphan that nothing needs and that needs nothing is often a mistake.
  - 8.2.3 Parallel candidates have no path between them and no shared files (§6.8).
- 8.3 **Hard vs soft.** Hard: B cannot correctly start without A’s output. Soft: “nice if A lands first,” a scheduling hint that must not freeze the run.
- 8.4 **Milestones** are where many arrows meet: natural full gates and review points (§12).
- 8.5 A cross-project need is an explicit edge (“needs the other project’s milestone X, because …”). The worker raises **blocked** rather than inventing a private format.
- 8.6 **Anti-pattern — a mislabeled edge.** A real data need marked soft races. A mere preference marked hard kills parallelism and can deadlock (a fix-up that depends on the review which spawned it). The review/fix-up case has one rule: no arrow from a spawned fix back to its parent review.

> **Canonical story — parallel safety:** The send path and a docs-only task share no files, so they can run together. Two tasks that both touch reminder storage are serialized.

---

## 9. The Work Loop: How the Agent Actually Runs

- 9.1 **Session start, then one task.** Before picking work: confirm the workspace, read the git log and the journal *tail*, run the doctor or a small smoke of the app (§12). Then pick the next ready task, read the sheet and the task, do the work, run the gate, **commit only work that is green under the written gate policy** (§12.3), mark status, append a short journal entry. One task per round. A broken environment has to fail this start, or new work piles onto it. Public long-running harnesses use the same orient-then-smoke order (Anthropic, Nov 2025).
- 9.2 **One source of truth for runtime state.** Humans author the plan and the initial tasks. Runtime status changes go through **commands**, not freehand edits of the live list. Structured state is harder for a model to casually rewrite than a markdown plan; a public harness keeps a feature list in JSON and allows the agent to flip a pass flag, not to delete tests (Anthropic, Nov 2025). Same split here.
- 9.3 **Status, one meaning each.**
  - To-do: ready when its hard edges are done.
  - In progress: claimed by this run.
  - Done: acceptance criteria met under the gate policy.
  - Failed: tried, criteria missed; counts toward escalation.
  - Skipped: deliberately deferred, not a failure.
  - Blocked: waiting on a human or an external fact; can resume (§11).
  - No-longer-needed: the scope moved.
- 9.4 **Progress journal:** about ten lines — approach, files, one or two lessons. Later rounds read the tail or search one task ID.
- 9.5 **Parallel work** only across different files, each worker in its own workspace, with a careful merge-back. Claiming a task has to be atomic. Two public lessons, both dated: a lock file plus git sync worked for a 16-agent compiler effort (Anthropic, Feb 2026); in another large run, locks were held too long or never released, and optimistic concurrency (the write fails if state changed since the read) was more robust (Cursor, Jan 2026). With no one assigning the hard tasks, agents drifted toward small safe edits. Planners assign, workers finish the assigned task, a judge decides whether to continue (§6.9, §12.5).
- 9.6 **Stop** on a time, money, or try budget, or when every task is done, failed, or blocked and no fix-up is still open. “Feels done” is not a stop condition.
- 9.7 **Overnight contract** — all of these are true before you walk away:
  - Every task has acceptance criteria and sits inside the current size bounds.
  - Human gates are flagged before the run, and the blocked exit is real (§11.6).
  - Destructive tools are denied (§13.4). The lethal-trifecta check (§13.5) has been done.
  - A budget will halt the process.
  - The morning artifact is a status list plus commits, not a chat to reconstruct.
  - A person reviews the milestone diff before it merges (§12.5). Agent review is not that review.

> **Field note — hand-edit split-brain:** An operator edits the live task file. The loop re-imports its own view. Tasks “vanish.” Commands only for runtime state.

> **Canonical story — journal:** A later task searches “TASK-027” and gets “reminders in their own table; times stored in UTC; convert at display.”

> **Field note — parallel ghosts:** After merge, deleted worker checkouts leave cached builds pointing at dead paths. The failures look like logic bugs. Rebuild before trusting them, and record the scar (§14).

> **Field note — claim mechanism:** Prefer a claim that fails closed when two workers race. A lock that is not released is a stuck run. A missing assignment function is a run that only does easy tasks. Pick the failure you can see.

---

## 10. Right-Sizing the Brain: Matching Capability to the Task

- 10.1 Not every task needs the strongest model.
- 10.2 **Model routing** is a config table: difficulty × role (build, review, plan, fix-up) → capability. Tasks carry difficulty and role only (§6.10). Models change in one table. Role matters because a strong reviewer and a cheap fix-up are different jobs; public runs that assigned one model to every role did worse than runs that split them (Cursor, Jan 2026). Do not copy a vendor’s current ranking into this paper.
- 10.3 Reviews and milestone gates default to the strongest available model. A weak reviewer waves through the miss the review exists to catch. Even a strong monitor is not a substitute for tests: on a realistic cheating benchmark, model monitors caught well under all of the cheating (ImpossibleBench). Trap tests and the blocked exit still have to be there.
- 10.4 **Escalation ladder:** on a real product failure, a stronger model before give-up. Record the promotion so the run does not ping-pong.
- 10.5 **Provider down** is not a task failure. Same capability tier on another provider when one is configured. An outage must not rewrite the routing table, and it must not count against the task (§13.2).
- 10.6 **Cost, as categories.** Tokens go to interview, worker tasks, review, milestone gates, and retries. Routing targets worker tasks. Thrift (§7) targets all of them. Ceremony that prevents a wrong multi-hour feature can be worth more than the feature’s tokens; ceremony on a 15-minute chat is not (§17). A dated calibration, not a price list: one public three-role harness moved a small app from about 20 minutes and $9 with no harness, to about 6 hours and $200, then to about 3 hours 50 minutes and $125 after the harness was revised (Anthropic, Mar 2026). The lesson is that structure has a price and that revising it can cut that price. Re-check any dollar figure at draft time.

> **Example — routing:** A help-text typo is an easy build on a cheap model. A double-claim race is a hard build on the top build tier. A milestone review uses the review tier even when the diff looks easy.

> **Anti-pattern — model names in tasks:** Sixty tasks say “use model X-2.” X-3 ships. The lists are wrong. Difficulty, role, and one table survive.

> **Field note — launch path down:** The vendor CLI’s start path breaks. With a same-tier fallback the run slows and continues. Without it, every task blocks. A half-day spike (§2) is how that bump gets sanded before the overnight.

---

## 11. Humans in the Loop: When the Agent Should Wake You

- 11.1 People own security trade-offs, spend limits, hard-to-undo choices, and open-ended research that has passed its timebox (§2). Flag those tasks before the run.
- 11.2 **Human-decision tasks** are marked needs-human. The agent **blocks** instead of guessing.
- 11.3 Other ready tasks that do not depend on the answer keep moving. The graph pauses an edge, not the whole run.
- 11.4 The human answer is machine-readable, and every downstream task that embedded the *proposed* value is updated in the **same change**.
- 11.5 A partial answer re-blocks with the remainder. Silence past the run’s human wait leaves the task blocked with a reason. The agent does not invent the rest.
- 11.6 **Blocked signal:** a standard “I’m stuck, and why,” and it has to be a legitimate success path for the agent. When tests and the spec disagree, models cheat by editing tests, special-casing, or overloading equality (ImpossibleBench, Oct 2025). Giving an explicit “flag for a human” exit cut the cheating rate sharply in that study (GPT-5 from 54% to 9%, o3 from 49% to 12%). Prompt wording swung one model from over 85% cheating to about 1%, which is why prohibited outcomes live on the standing sheet (§7), not in a one-off reminder. Quote the study with its date; the design lesson is the exit, not the percentages.
- 11.7 The interview (§4) and these tasks are the same idea at two times: before planning, and during the run.
- 11.8 **Name the humans:** plan owner, person who may resolve a blocked decision, person who merges. One person may hold all three. An unnamed gate is how an overnight run waits until Monday with no alert. The merge owner reviews the milestone diff (§12.5). That is a different job from the agent’s review task.

> **Canonical story — send cap:** The proposed “100/day” affects billing, so the task is needs-human. The agent blocks and continues elsewhere. The human chooses 60. The outcome and every downstream “100” update together.

> **Anti-pattern:** The decision lives in chat. The tasks still say 100. The agent implements 100.

---

## 12. Checking the Work: Verification and Quality Gates

- 12.1 “Done” is a claim. Passing tests are not the end of the claim: a large parallel effort put it as “it is easy to see tests pass and assume the job is done, when this is rarely the case” (Anthropic, Feb 2026).
- 12.2 **Quality gates:** tests, formatters, linters, type checkers.
- 12.3 **Two speeds.**
  - 12.3.1 Per task: a fast **scoped** gate on what this task touched. Prefer harness output that prints a few lines and writes the rest to a log, so the gate does not blow the context window.
  - 12.3.2 Per milestone: the **full** project gate.
  - 12.3.3 **Pre-existing red,** one policy per team, written on the sheet. *Strict:* the milestone leaves the trunk green, including old failures. *Pragmatic:* a **known-fail allowlist** (named test, owner, ticket) may stay red; everything new or touched is green. An unlisted failure is not “someone else’s mess.”
  - 12.3.4 A large unrelated red pile: fix what this project caused, file one task for the rest, block for an owner. No silent skip, and no week spent on another team’s backlog.
  - 12.3.5 The §9.1 commit rule means “green under *this* policy,” not “the entire history of the repo is green” unless the team picked strict.
- 12.4 Bugs: failing test, then fix, then pass.
- 12.5 **Two reviews, different jobs.**
  - The agent **review** task looks and does not fix. Its sharpest question: is the new code on the real path? Green unit tests with doubles hide a function that is never registered.
  - A **person** reviews the milestone diff before merge. Agent review is not merge approval (§1.8, §9.7).
  - When a trusted reference implementation exists, compare against it (**oracle** checking). The C-compiler effort used GCC this way so agents could repair different files in parallel (Anthropic, Feb 2026). Use an oracle when you have one. Do not block ordinary product work on inventing one.
- 12.6 Fix-ups re-enter the graph with the §6.9.4 anatomy and with arrows that do not point back at the parent review (§8.6).

### 12.A Proving the user path: the verification skill

> Code gates feel the joints unit tests can feel. A **verification skill** records the bumps a user would hit. A passing drive is **evidence**, not a universal proof — the same caveat property-based suites deserve (§6.5.3).

- 12.7 Language gates prove the code. A change a person can click or type still needs a drive of the real app: start it, exercise the feature the way a user would, keep the evidence. Telling a long-running agent to test in a real browser, the way a human would, improved outcomes in a public harness (Anthropic, Nov 2025). That is this section’s job. Do not prescribe a browser tool by name.
- 12.8 The skill is a project-local sheet the *next* agent can follow cold:
  - **Launch** — the repo’s own start, or for a short-lived CLI, this checkout’s binary in an isolated session. How to tell it is ready.
  - **Doctor** — read-only: right build, right data directory, not the operator’s real session. Refuse to drive on failure. The §9.1 smoke is this check.
  - **Drive** — stable handles from this repo, not coordinates or a harness invented for one task.
  - **Evidence** — the action and the resulting state, including side effects. A dry-run counts only after you observe what it skipped.
  - **Cleanup** — stop what this run started. Never kill by process name. Proof artifacts stay.
- 12.9 **Feature map:** one short file per user-facing feature (what it is, how a user reaches it, how to drive it, what end state proves it, gotchas). Start with the top few. A proof of one convenient entry point is incomplete when the map lists others.
- 12.10 Interview the repo to write it. If the checkout does not start, fix that first. Then run the skill once. An unexecuted skill is a draft.
- 12.11 Maintain the map when user-facing behavior changes. A stale map false-greens the next phase. Upkeep reads source and drives live, fixes the map and the harness, and reports a product bug instead of editing the map until the bug looks intended. A drive of a foreign instance (the `PATH` binary, the operator’s home, last week’s server) proves nothing.
- 12.12 In the loop: language gate, then the feature recipe, for tasks that touch a mapped feature. Pointer, not a paste (§7). Internal refactors stay on the language gate. New user-facing behavior updates the feature file in the same change.
- 12.13 Each gotcha written down is a bump the next lap does not rediscover. The throughput claim is §16.7. Do not repeat it here.

> **Canonical story — user-path proof:** `send()` is unit-green. The reminders feature file requires an isolated app, one trigger, one captured send, a second trigger, and no second send. Nothing is registered, so the evidence fails and a wiring fix-up is spawned. The transcript survives cleanup. The feature file gains the gotcha “scheduler must register the sender.”

> **Field note — the wrong train:** Drives kept using whatever binary was on `PATH` and the operator’s real config. Doctor now refuses unless the binary is the one just built and the data directory is a sandbox.

> **Anti-pattern — a private harness per task:** Ad-hoc scripts, coordinate clicks, or internal setters. The suite is green. The next task derails on a different bump.

> **Anti-pattern — stale map:** The file still says the screen lives under Settings. The next run follows the file. Maintenance drives the live app. A real regression becomes a bug, not a doc edit that hides it.

---

## 13. Handling Failure: Because Things Will Go Wrong

- 13.1 Respond in order: **retry → escalate → reroute → block → ask a human** (Appendix F).
- 13.2 **Classify before you punish.** Flaky network, bad credentials, and a product bug are different. Only the product bug counts against the task. A provider outage uses §10.5.
- 13.3 **Protect shared state:** snapshot orchestrator-owned files before a risky step; restore if corrupted.
- 13.4 **Guardrails in tools, not memos.** Allow untrack, refuse physical delete. Refuse test-file edits if the agent’s job is to pass the tests, knowing this stops only one cheating move (§6.3.2). A **hook** is the usual shape: a deterministic check that runs on a tool call and can deny it, independent of whether the model “remembers” the rule. One vendor’s hook format is an implementation, not the principle. A public parallel run included an agent that killed every shell and took its own loop with it (Anthropic, Feb 2026). That is the same class of bug as deleting a file the human meant to untrack.
- 13.5 **Security boundary for overnight runs.**
  - Secrets never go in plans, tasks, journals, or prompts committed to the repo.
  - Destructive operations (drop a database, force-push, mass delete) are human-decision tasks or hard denials.
  - **Lethal trifecta** (Willison): private data, untrusted content, and a channel to communicate outward. If a run has all three, remove one before you sleep. Coding agents are where current incident write-ups concentrate (OWASP-related reporting, 2026; confirm the primary report URL at draft time).
  - Untrusted content is the repo, issue text, dependency docs, and tool output, not only the plan. Trusted humans edit the plan. That does not make a comment in the code safe.
  - Least privilege on credentials. No standing production credentials on an overnight run.

> **Example table:**

> | Failure | Response | Counts against the task? |
> |---|---|---|
> | Provider 502 | wait, then retry | no |
> | Invalid credentials | pause the run, alert a human | no |
> | Same product test fails twice | escalate or block with a note | yes |
> | Agent edits the test to go green | tool denial; treat as a failed proof | yes |

> **Field note — snapshot/restore:** The agent “cleans up” settings. The post-check restores the snapshot. The corruption stops there.

> **Field note — delete vs untrack:** Asked to untrack, the agent deleted the file on disk. The fix was a tool denial, not a sharper sentence.

---

## 14. Memory: Learning From Every Project

- 14.1 After a success, a failure, or a workaround, record one or two lines: what, why, how to apply. Short notes get read.
- 14.2 **Recall** at plan time (§3.5), at task design (§6.11), and at session start: files, task type, error strings; tags and free text.
- 14.3 **Hygiene:** retire what is stale, merge duplicates, and remember which notes were actually used so those surface. Summarizing long-closed work is legitimate hygiene. Rewriting the whole memory is not.
- 14.4 **Two ways memory rots, besides a wrong lesson.** *Brevity bias*: a summary drops the detail that made the lesson usable. *Context collapse*: rewriting the same memory over and over wears specifics away (ACE, Oct 2025). **Rule — append and merge lessons. Do not let an agent rewrite the memory file from scratch.** A wrong lesson still needs an explicit invalidate path. A confidently repeated false scar trains every later run.
- 14.5 **Decision log:** the choice and the rejected option (§7.5). Future work should not relitigate it in a fresh spike.

> **Field note — the ghost path:** “A mass of failures naming a deleted workspace → rebuild before debugging.” The next run matches the error and rebuilds in minutes.

> **Example — decision log:** One shared database per project, not one file per feature (parallel workers collide on those files), and not cloud-only (the tool has to work offline).

---

## 15. Putting It All Together: The Full Cycle

- 15.1 Do not write a second procedure. Point at the §1.9 figure. The story below is the one full pass.
- 15.2 **Health signals** (definitions in §18): bounces are rare; humans were flagged before the wrong implementation; the same user-path gotcha is not rediscovered after it hits the feature map; a blocked exit was used instead of a green cheat; milestone red is either new or on the allowlist.

> **Canonical story — email reminders:**
> (1) Spike: one real send; save success and failure; one-day timebox.
> (2) Plan: 24h before due; never double-send; no SMS; cut custom sounds; stop-sign on the daily cap, resolved as batching. Verdict only — the spike already compared approaches.
> (3) Interview: time zone and edit-after-queue, written back. The fork does not leak into the sheet.
> (4) Contract: reminder record, verified read, double-send invariant owned.
> (5) A handful of small tasks, a trap test, a needs-human task for the cap. The sheet has the priority order, the contract pointer, prohibited outcomes, and a few lessons.
> (6) Graph: dependents of the contract; send path and docs share no files.
> (7) Run: session start smokes the app; easy build on the cheap tier; scheduling on the strong build tier; block for the human; journal ~10 lines.
> (8) Verify: scoped gates under the written policy; the feature file fails because nothing is registered; wiring fix-up; re-drive until one captured send and no second send.
> (9) The feature file records “scheduler must register the sender.”
> (10) Learning: “the provider silently drops bodies over 1 MB,” appended, not rewritten over older notes. The next spike checks that on day one. Evidence rules already say to read the provider response, not only a local “queued” flag.

---

## 16. Why This Design Pays Off

> The method is already complete in §15. This section is the system effect, each tied to a mechanism from earlier. Do not introduce a new practice here.

- 16.1 **The graph buys order and speed from one picture.** Arrows enforce foundations-first. Absence of an arrow, plus fresh file facts, is what makes parallelism safe.
- 16.2 **A fresh mind for every task.** Each task is a **context reset**: a new session whose memory is the plan, the sheet, the journal, and git. That is a deliberate choice against **compaction**, which summarizes the old conversation so the same agent can continue and, in public harness write-ups, still does not give a clean slate (Anthropic, Mar 2026). **Context rot** (§7.3) is why a fifty-task chat gets worse while the window still “has room.” The Ralph loop (Huntley; Carson’s variant keeps `progress.txt` and a task JSON) is an independent public example of the same bet: fresh context, backpressure from tests, files as memory. The reset is not free. It reloads the handoff. Thrift (§7) exists so that reload stays smaller than the errors a long chat accumulates. Do not claim the reset is unconditionally cheaper.
- 16.3 **The run gets smarter the same day.** A lesson after task 5 is in the sheet for task 6, by append (§14.4), not at the next retrospective.
- 16.4 **The journal is how the next session finds “how we got here”** without replaying the chat.
- 16.5 **The next project’s plan starts with the scars already in it** (§3.5).
- 16.6 **Reviews, tests, and cleanup are tasks with IDs and arrows.** They are not “when we have time.”
- 16.7 **Lap count is the payoff (callback to §1.5, do not retell).** Hand-pulled work caps at the hours you are awake. The self-propelled run caps at the track: plan, contracts, gates, journal, and the verification skill. Early laps are slower because you are sanding. After the bumps are written down, you stop touching every lap, and you will not match that lap count by hand. A stale map puts a bump back. A piece of ceremony whose failure no longer happens is itself a drag; lift it (§17.6).

---

## 17. When to Use This, When to Skip It, and How to Adopt

### 17.1 Decision table

| Situation | Use |
|---|---|
| Single concern, you can watch, “done” is obvious | **Conversation.** No plan document, no graph. |
| A few files, clear acceptance criteria, you are still interactive | **Light task list** (titles + acceptance criteria). Skip spike, interview, and contracts unless a shared connector appears. |
| Multi-hour or overnight, ambiguous done, dependencies, or you will be offline | **Full cycle.** |
| Open-ended research, no product change yet | **Spike only** (§2). |
| Requirements still moving week to week | Spike plus a light list. A frozen graph fights discovery. |
| A one-off script you will throw away | Conversation or light list. |

- 17.2 **Rule of thumb:** If explaining “done” takes longer than doing the work, you over-ceremonied. If the agent has declared done twice and you still cannot list leftovers, you under-ceremonied. Fowler’s walkthrough of spec-driven kits (spec-first vs spec-anchored vs spec-as-source) found the heavy markdown path overkill on small work and worried it creates a false sense of control. This table is that critique, answered: match the artifact to the job, and keep a paragraph plan when a binder would only be re-read.
- 17.3 **Adoption**
  - **Week 1:** One medium project, on the order of 10–20 tasks. Plan, tasks, scoped gates. No parallel, no second provider. If the app has a user-visible surface and no way to drive it, this week may instead be “write the verification skill and prove one feature.” That lap is allowed to feel slower than pulling the train by hand.
  - **First larger effort:** Interview write-back, one contract, one milestone gate, one real human-decision. User-facing slices run the feature recipe after the language gate.
  - **First overnight:** Budgets, the blocked exit, journal discipline, the routing table, the overnight contract (§9.7).
  - **Ongoing:** Recall into plans; append-only lessons; maintain the feature map when a user-facing behavior changes; parallel only while file facts are fresh.
- 17.4 **Name the roles** in §11.8 before the first overnight. The epic can stay in the team’s ticket tracker. The executable list is the agent’s source of truth. Status flows back as done, blocked, or fix-up. The list does not replace the tracker for humans, and the tracker does not replace the list for the agent.
- 17.5 **Limits of a well-run ceremony.**
  - Same-family interviewer, author, and implementer share blind spots (§4.7).
  - File-touch facts go stale (§6.8).
  - Two runs of the same list produce different diffs. The process repeats. The patch does not.
  - A passing user-path drive is evidence for the path you drove (§12.A).
  - Fast-moving discovery belongs in the table above, not in a stricter plan.
- 17.6 **Rule — re-audit the ceremony when the model changes.** Every piece encodes an assumption about what the model still gets wrong: context resets, sprint-sized chunks, size bounds, a strong-model review, a particular hook. Check whether that failure still happens. If it does not, remove the piece. A public harness author dropped forced context resets after one model generation and dropped sprints after the next (Anthropic, Mar 2026). The same write-up’s warning applies here: too little structure and agents conflict, duplicate, and drift; too much structure is brittle (also Cursor, Jan 2026, after removing an integrator role). The wooden track should lose a sanding block that the wheels no longer catch.

> **Example — good first project:** An export feature on an internal tool, about 10–20 tasks. Big enough for one contract and one review. Small enough that a miss costs an afternoon.

---

## 18. Measurement: Is the Workflow Working?

> The feature shipping is a different question from the workflow working. The status store and the journal already hold bounce, escalation, and blocked reason. Do not build a metrics product before the first run.

- 18.1 **Start with four:**
  - **Bounce rate** — done, then reopened or failed again.
  - **Flagged-in-advance share** — of human interrupts, how many were marked needs-human before the run, versus surprises after a wrong implementation.
  - **User-path catch rate** — user-facing tasks whose feature-file drive failed after the language gate was green. High early means the map is finding bumps. The same gotcha falling after it is written down means the bump stayed sanded. A catch rate that falls while maintenance has not driven the app lately means the map may be stale.
  - **Escaped defects after merge** — what a lead actually feels. The workflow is there to push this down, not to maximize tasks closed per day.
- 18.2 Direction, not a universal target: bounces down, flagged-in-advance share up, repeated user-path misses of a recorded gotcha down, escaped defects down. Optional counts (tokens per task, parallel wall-clock, plan edits after interview, memory hits, laps per human hour, whether the morning diff fits one review sitting) live in Appendix H.
- 18.3 **Do not optimize** raw tasks closed per day, or zero human interrupts. The first rewards empty tasks. The second rewards guessing, which is the cheating result §11.6 is trying to avoid.

---

## 19. Conclusion

- 19.1 **Thesis:** Coding agents do their best long-running work when a human designs the system — clear goals, small checkable steps, honest states, scheduled human gates, and memory outside the chat — and the agent executes the steps under **bounded autonomy**.
- 19.2 **Three non-negotiables:**
  1. **The plan is the source of truth.** Interview decisions land in the plan. Chat is not memory.
  2. **Tasks are small and checkable.** Acceptance criteria, trap tests, one task per fresh session.
  3. **Gates, a user-path track, and short memory.** Scoped and milestone checks, a verification skill for anything a person can click or type, and lessons appended so the next lap is smoother.
- 19.3 **Skipping a piece has a recognizable result:**
  - No write-back → a faithful wrong implementation.
  - Reviews that implement inline → no graph, no proof, scope creep.
  - No trap test and no blocked exit → a green suite that cheated.
  - No verification skill on a user-facing app → every task re-surveys the track, or ships a function nothing calls.
  - A skill never maintained → the next phase false-greens.
  - Model names baked into tasks → rot on the next vendor release.
  - Full ceremony on a 15-minute job, or ceremony you never re-audit → the track is heavier than the train. People go back to pulling by hand.
- 19.4 **This week:** one forgiving project. A short plan, a three-question interview written back, sized tasks with acceptance criteria, scoped gates, a one-page sheet. If users can see the change, prove one feature through the real app and keep the evidence. Compare that morning to the last big chat that went sideways.
- 19.5 Add a piece when its absence hurts: a missing contract, a surprise human decision, an unmeasured overnight. Remove a piece when a model upgrade makes its failure rare (§17.6).

---

## Appendices

- **A. Glossary** — every bold term in plain language: coding agent, workflow, bounded autonomy, hypothesis, contract, invariant, acceptance criteria, trap test, property-based test, reward hacking, DAG, hard/soft dependency, milestone, model routing, escalation ladder, blocked signal, quality gate, scoped vs full gate, known-fail allowlist, verification skill, feature map, context thrift, context rot, compaction, context reset, standing instruction sheet, AGENTS.md, progress journal, learning, brevity bias, context collapse, decision log, oracle, hook, lethal trifecta. Sprint contract: see acceptance criteria; do not define a second term. Verification skill: project-local instructions an agent can follow cold to launch the real app, drive one user-facing feature, and keep evidence. Feature map: one file per such feature.
- **B. Task template** — ID, title, difficulty, role, files touched, dependencies, acceptance criteria, pre-check, completion check, time budget, notes, needs-human. Filled sample: TASK-012.
- **C. Starter interview questions** — fuzzy words, limits, conflicts, hard-to-undo, “what happens when…?” Optional: an EARS-style “when / the system shall” sentence for an invariant you intend to property-test. A seed list, not a script.
- **D. Plan completeness checklist** — quality; ≥2 named edges; ≥1 rejected approach; top 3 risks; consumers; deliberate cuts; documentation plan or “none — on purpose.”
- **E. Task-list and graph checklist** — every story present; every edge has a test; a trap test per task; size bounds treated as current, not eternal; no model names; human flags; sheet complete and distinct from AGENTS.md; user-facing tasks point at a feature file; no cycles; fix-up arrows do not point at the parent review; hard and soft are not swapped; parallel tasks share no files.
- **F. Failure ladder** — retry → escalate → reroute → block → human, with the §13 table.
- **G. When-to-skip matrix** — §17.1 expanded with one example per cell, including “requirements still moving.”
- **H. Metrics starter** — the four in §18, plus optional counts and how to read them from the status store and the journal. Healthy direction, not universal targets.
- **I. Positioning map** — one row each, not a literature review. Say what this paper keeps from each.

  | Lineage | What it contributes | What this method adds or refuses |
  |---|---|---|
  | Work breakdown, ticket trackers | Hierarchy and status for humans | The agent’s executable list and acceptance checks; the tracker stays the human epic (§17.4) |
  | Free-roaming agents | Autonomy | A durable leftover list and a handoff; no undeclared “done” |
  | Spec-driven tools (Kiro, Spec Kit, Tessl; Fowler’s spec-first / spec-anchored / spec-as-source) | Requirements before code; Kiro also runs independent tasks together and can generate property tests from requirements | Spec-anchored, not spec-as-source. Paragraph-sized plans for small work (§17). Property tests are optional evidence |
  | Long-running harnesses (Anthropic 2025 initializer + session ritual; Anthropic 2026 planner / generator / evaluator; Ralph; Cursor’s planner / worker / judge) | Feature list, progress file, fresh sessions, generation separated from evaluation, a loop that may not stop early (Osmani’s summary of the pattern) | Same bones. Ceremony is re-audited per model (§17.6). User-path proof is a maintained feature map, not a one-off browser instruction |
  | Graph trackers built for agents (Beads and similar) | Dependencies as data, ready-set computed, cycle checks, memory decay of old closed items | The principle §6.8 / §8 / §14.3. The paper does not standardize on one tracker |

- **J. References** — URLs and dates for every named public source. Re-verify before publication. Body text stays at “name, month year.”

  - Anthropic, “Effective harnesses for long-running agents,” Nov 2025. https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents
  - Anthropic, “Harness design for long-running application development,” Mar 2026. https://www.anthropic.com/engineering/harness-design-long-running-apps
  - Anthropic, “Effective context engineering for AI agents.” https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
  - Anthropic, “Building a C compiler with a team of parallel Claudes,” Feb 2026. https://www.anthropic.com/engineering/building-c-compiler
  - Cursor, “Scaling long-running autonomous coding,” Jan 2026. https://cursor.com/blog/scaling-agents
  - ImpossibleBench, arXiv:2510.20270, Oct 2025. https://arxiv.org/html/2510.20270v1
  - Agentic Context Engineering (ACE), arXiv:2510.04618, Oct 2025. https://arxiv.org/pdf/2510.04618
  - Martin Fowler, “Understanding Spec-Driven-Development: Kiro, spec-kit, and Tessl.” https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html
  - Kiro specs and correctness. https://kiro.dev/docs/specs/ and https://kiro.dev/docs/specs/correctness/
  - Ralph loop: https://ralph-wiggum.ai/ and Carson’s variant https://github.com/snarktank/ralph
  - Addy Osmani, “Long-running Agents.” https://addyosmani.com/blog/long-running-agents/
  - AGENTS.md. https://agents.md/
  - METR time horizons, Mar 2025, and Time Horizon 1.1, Jan 2026. https://metr.org/blog/2025-03-19-measuring-ai-ability-to-complete-long-tasks/ and https://metr.org/blog/2026-1-29-time-horizon-1-1/
  - Simon Willison, “The lethal trifecta for AI agents,” Jun 2025. https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/
  - OWASP / 2026 agent-incident reporting — confirm the primary report URL at draft time. Secondary: https://www.helpnetsecurity.com/2026/06/11/owasp-prompt-injection-ai-security-failures/
  - Hooks as one implementation of §13.4. https://code.claude.com/docs/en/hooks-guide
  - Beads (graph tracker example for Appendix I only). https://github.com/gastownhall/beads

---

## Story index (drafting)

| Name | Where |
|---|---|
| Wooden track | §1.5; callback §16.7, §17.6, §19 |
| Billing “done!” too early | §1 |
| Email reminders | §2–§6, §8–§9, §11–§12, §15 |
| Date format, two purposes | §3 |
| Interview without write-back | §4 |
| Five shapes of a record | §5 |
| Hand-edit split-brain | §9 |
| Parallel ghosts; claim/lock field note | §9 |
| Wrong binary / stale feature map | §12 |
| Delete-vs-untrack; snapshot restore | §13 |
| Ghost-path learning | §14 |

---

## Drafting order

1. Abstract, §1, and §17, so audience, the train, and the re-audit rule lock the voice.
2. §15 story, so later snippets cannot drift.
3. §3–§7, including the fact-home table before any section tries to store a fact twice.
4. §9–§14. Session start before the verification skill, so doctor has a caller.
5. §16, §18, §19, appendices.
6. Figure and glossary last. Check every dated number in Appendix J still says what the body claims.
