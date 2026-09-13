# Whitepaper Outline: How to Keep a Coding Agent Working While You Sleep

**Working title:** *Teaching a Robot to Finish Big Jobs*

**Subtitle:** *A practical methodology for long-running coding agents — so the work still points the right way at 3 a.m.*

**Audience:** People who already use AI for coding (Claude Code, Grok Build, Codex, Cursor, etc.) and can get small jobs done in a 10–15 minute chat — but lose the plot when the job is big: multi-file features, multi-day refactors, “finish this while I’m offline.” This paper is for that second class of work.

**What this is not:** A model comparison, a prompt catalog, or a general “AI for everything” workflow. It is a software-engineering methodology for **bounded autonomy** (supervised long runs), not full autopilot.

**When *not* to use this (preview — full decision table in §17):** If the job fits in one focused conversation and you can watch it, just chat. The ceremony below pays off when the work is large enough that “done” is ambiguous, steps depend on each other, or you will not be at the keyboard for every decision.

---

## Design notes for drafting (not part of the published paper)

- **One canonical story:** “Add email reminders to the notes app.” Snippets appear in earlier sections as *Field note — continued*; the full walkthrough is assembled once in §15.
- **Callout types:** **Rule** · **Anti-pattern** · **Field note** (real production systems, anonymized as “a task-workflow tool we ran”).
- **Principle vs tool:** Body states principles; tool-shaped failures are *Field notes*, never “you must implement our CLI.”
- **Plan = what/why; tasks = how/checkable.** Named principle; enforce in §3 and §6.
- **Externalized memory over chat history.** Fresh conversation per task is a thesis claim (§14 / §16), not a side tip.
- **Bolded terms** all appear in Appendix A (glossary).
- **Length discipline:** 1 primary example + 1 failure mode per major section in the body; extras live only as outline options below or in a case-study appendix later.
- **Figure after §1:** one-page map of the paper (spike → plan → interview → contracts → tasks → graph → loop → verify → memory → benefits).

---

## 1. Introduction: Why Coding Agents Need a Plan

- 1.1 What a **coding agent** is: a computer helper that can read a repo, edit files, run tests, and change systems with little or no hand-holding in the moment.
- 1.2 The core problem: agents are strong on small jobs (rename, fix a test, add a flag) and get lost on big ones. They forget earlier decisions, invent scope, declare “done” early, and have no honest answer to “what’s left?”
- 1.3 The big idea: don’t hand the agent one giant job. Give it a **workflow** — a written plan of small tasks, with rules, checkpoints, right-sized models, human stop points, and a **memory** outside the chat.
- 1.4 **Bounded autonomy (supervised autonomy):** the human designs the *system* (goals, checks, when to wake a person); the agent runs the *steps*. This is not “set and forget with no oversight” — it is “oversight is scheduled and machine-readable,” so you can sleep without the agent freelancing.
- 1.5 Who this paper is for:
  - Engineers and tech leads who already pair with coding agents daily.
  - People shipping multi-task changes (features, migrations, cleanups) who want overnight or multi-hour runs.
  - Not primarily for non-coding general agents or pure research chat.
- 1.6 What you will learn: a full cycle from spike to learnings; concrete size limits; how plans and tasks differ; when to skip the ceremony.
- 1.7 What this paper is *not*: vendor rankings; magic prompts; a replacement for code review judgment; a mandate to use any particular task runner.
- 1.8 Map of the paper (figure): discovery → plan → interview → contracts → tasks → graph → run → models → humans → verify → failure → memory → full cycle → benefits → when to use / adopt / measure.

> **Field note — the billing feature that “done!”’d too early:** A team asks an agent to “build our whole billing feature.” It works for an hour, says done — half the screens missing, tests red, no honest leftover list. Same team then breaks the job into ~20 small tasks with checklists. When the agent stops, the task list shows exactly what is done, what failed, and what’s left.

> **Anti-pattern:** Running the full methodology on a 15-minute typo fix. That’s a conversation, not a workflow.

---

## 2. Start With Discovery: Look Before You Leap

- 2.1 **Exploration spikes**: before planning a big project, run one small, cheap experiment that tests the **riskiest guess**.
  - 2.1.1 Write the guess as a **hypothesis** — a sentence that could be proven wrong.
  - 2.1.2 Build the thinnest possible version that tests it.
  - 2.1.3 Compare 2–3 approaches; pick one; write trade-offs. That heavy compare work is *not* repeated in the plan — the plan summarizes the conclusion.
- 2.2 Why it matters: wrong on day one is cheap; wrong on day twenty is not.
- 2.3 **Timebox risky research and write the fallback down:** every open-ended investigation gets a deadline and a pre-chosen default (“if library bake-off exceeds 3 days, ship library X and record why”).
- 2.4 **Anti-pattern — research theater:** unlimited exploration with no decision artifact. A spike’s output is a *decision* (and sometimes a **contract**, §5), not a pile of notes.
- 2.5 Output of this step: short learning note + optional contract seed for §5.

> **Field note — can we even launch the agent?:** A workflow tool assumed “start the vendor CLI, hand a task, read the answer” on a plain machine. A half-day spike would have surfaced that the launch path can break when the vendor changes it. Turn “we assume this works” into “we watched it work (and we know what breaks it).”

> **Field note — map the real API (canonical story starts here):** Before designing email reminders, spend one afternoon calling the mail provider. Save real responses for success, bad request, auth failure, and outage. Later failure-handling tasks (§13) list real error codes, not documentation folklore.

> **Example — timeboxed spike:** “Two days, three search libraries. No clear winner → keep the familiar one and write the tie note.” Day two ties; ship familiar; no three-week research hole.

---

## 3. Project Planning: Writing Down What “Done” Means

> **Rule — plan says *what* and *why*, not *how*.** The plan is the answer sheet for an agent working alone at 3 a.m. *How* (which files, which proof command, which trap test) belongs in task design (§6) and to the agent doing the work.

### 3.A What kind of job, and how big is the plan?

- 3.1 **Name the job kind.** Feature · bug fix · enhancement · cleanup (no behavior change). Different kinds need different questions (bug: reproduce + expected vs actual; feature: who for + smallest useful version).
- 3.2 **Match plan size to project size.** Paragraph for small multi-file work; full document for multi-day efforts. If the job is a single chat (see §17), skip this section entirely.
- 3.3 The first draft will have holes. The interview (§4) exists to find them — not to invent a perfect plan on try one.

### 3.B Look around before you write

- 3.4 Read the existing system: likely files, patterns to copy, edge cases the current code already handles (clues), alternative designs already present.
- 3.5 Check **team memory** (§14) for similar past work — search by topic *and* by keyword; each misses what the other finds. Paste relevant lessons into the plan so the worker does not repeat them.

### 3.C What a good plan contains (the core)

- 3.6 **Goal** — problem and for whom — plus **success measures**: numbers, not vibes (“search under 1s,” not “feels fast”).
- 3.7 **User stories** — “As a *[role]*, I want *[ability]*, so that *[benefit]*.”
- 3.8 **Rules / invariants seeds** — what must always be true; what must never happen (full **invariants** land in §5).
- 3.9 **Quality dimensions** — correctness, performance, style/patterns. Distinct from success measures: measures say *ship outcome*; dimensions say *how good the solution must be while shipping it*.
- 3.10 **Named edge cases** — concrete tricky inputs/situations, each with expected behavior. Named → agent handles; unnamed → customers discover.
- 3.11 **Failure edges** — bad input, missing files, network down (high level; detailed failure ladder is §13).
- 3.12 **Out of scope** — saying no on purpose.
- 3.13 **Deliberate cuts** — value-vs-effort: name the 1–2 vision pieces that cost most and help least; cut or defer *in writing* before task design.

> **Canonical story — plan core (email reminders):** *Goal* — users get a reminder 24h before a due date; *success measure* — send attempt completes under N seconds; *story* — “As a note-taker, I want a timely reminder so I don’t miss deadlines.” *Rules* — never send the same reminder twice. *Edges* — provider down → retry with backoff, never silent drop of the user’s intent. *Out of scope* — SMS. *Deliberate cut* — custom reminder sounds (demo candy). *Stop-sign risk* — provider’s daily send cap (see §3.E).

> **Example — quality + named edge (search, optional secondary):** Correctness includes “café” matches “cafe”; empty search shows all notes, never errors. Naming the café case is what makes the agent handle it.

### 3.D Compare approaches, then stop

- 3.14 **Compare 2–3 approaches** (or summarize the spike’s conclusion). For each: strengths, weaknesses, verdict. Keep at least one *rejected* option with reason so nobody re-argues it.
  - 3.14.1 Prefer long-term foundations over short-term speed when rework would compound.
  - 3.14.2 Combining best parts of two approaches is fine — say so explicitly.

### 3.E Plan hardening (keep short in the body; detail in Appendix D)

- 3.15 **Consumers of changed behavior.** For behavior changes: list every caller/use; mark fine / breaks / needs look. Watch *same code, different purposes* — split the work if the “fix” is right for one use and wrong for another.
- 3.16 **Top 3 risks** via inversion: “How would this design *guarantee* failure?” Rank damage × likelihood. **Stop-sign rule:** big *and* likely → human decides before planning continues.
- 3.17 **Documentation plan.** Which guides/architecture overviews change — or write “none” on purpose.
- 3.18 **Completeness check** (see Appendix D): quality filled? ≥2 named edges? ≥1 rejected approach? risks? every section filled or marked “none — on purpose”? Blank = forgotten.

> **Field note — same function, two purposes:** Date-format “fix” breaks file-naming callers that cannot accept the new character. Plan splits: UI format vs filename format.

> **Canonical story — stop-sign risk:** Mail provider allows 100 emails/day; user count makes that likely. Human must choose plan upgrade or batching before task design continues.

---

## 4. The Interview: Let the Agent Ask Before You Design Tasks

- 4.1 First drafts hide blind spots. Missing requirements are almost never intentional — they are questions nobody asked.
- 4.2 **Plan interview:** give the draft plan to an agent whose *only* job is clarifying questions (good contractor walking the house before the quote).
  - 4.2.1 Probe: fuzzy words (“fast,” “secure”), unstated limits, conflicting rules, hard-to-undo choices, “what happens when…?”, unnamed edges, missing quality targets, low-value scope that should be cut.
  - 4.2.2 Ask 3–5 questions per round, not fifty.
  - 4.2.3 Prefer **multiple-choice** answers + a suggested default when the human is unsure.
  - 4.2.4 Human answers — or parks “needs a human decision task” (§11).
- 4.3 **Role separation:** interview agent ≠ worker agent. The interviewer is allowed to wander; the worker only sees the *updated plan*.
- 4.4 **Forked conversation:** Q&A happens in a branch/disposable chat.
  - 4.4.1 Exploration is valuable; transcript noise is poison to later workers.
  - 4.4.2 **Rule:** the conversation is disposable; the **decisions** are not.
- 4.5 **Write decisions back into the plan** before task design.
  - 4.5.1 Every answer that changes a rule, limit, or cut is edited into the plan document.
  - 4.5.2 **Rule — if it isn’t in the plan, it didn’t happen.** Workers read the plan, not chat history.
- 4.6 Done when another question round stops changing the plan. Typical: 2–3 rounds. Ten rounds means planning started too early (return to spike or shrink scope).

> **Canonical story — interview:** Draft says “remind 24 hours before.” Interviewer asks: time zone of “24 hours”? What if the user edits the due date after a reminder is queued? Shared/private notes? Answers written back into the plan; long format tangents stay in the fork.

> **Anti-pattern — interview without write-back:** Great Q&A; original plan never updated. Weeks later a worker builds the wrong export/privacy behavior because that was the only document it could see.

---

## 5. Boundary Contracts: Agree on the Connectors First

- 5.1 A **contract** is a promise about how two pieces of work fit together (data shape, function names, interface rules).
- 5.2 Why first: if five tasks plug into one connector, design the connector before the five — or they guess five ways.
- 5.3 Contracts as tasks: any connector **two or more** downstream tasks will build against gets its own contract task, named in the plan; dependents point at it in the graph (§8).
- 5.4 **Invariants:** rules that stay true the whole time (“never double-save,” “money never negative”). Written down; checked every step.
- 5.5 Both sides of the deal: inputs (incl. bad input), success *and* failure outputs, side effects. If modifying an existing connector: what breaks for current users and how they migrate.
- 5.6 **Data-shape contracts — show a verified, copy-paste example**, traced from the real system, not guessed. Wrong shape often *looks* right, runs clean, returns empty — worst bug class.
- 5.7 Sources: many contracts surface in the interview. Keep new connectors few; each concept has exactly one owner module.
- 5.8 **Clarify the trio (diagram in draft):**  
  - **Contract** = shared interface between parts  
  - **Invariant** = always-true rule across the system  
  - **Acceptance criteria** = per-task yes/no done checks (§6)

> **Canonical story — contract:** Define the reminder record (task/note ID, send time, sent flag) once, with a verified read example, before send/UI/import tasks start.

> **Field note — five shapes of “task record”:** List UI, next-task picker, progress report, importer, and reviewer all invent slightly different records without a contract; merge is a mess.

> **Field note — silent nested options:** Settings “options” section uses different access rules than top-level fields. Guessed code looks perfect, always empty; no synthetic test catches it. Verified example at each hop prevents it.

---

## 6. Task Design: Breaking the Plan Into Bites

- 6.1 What makes a good task:
  - 6.1.1 Small enough for **one sitting** — **one task per loop round**.
  - 6.1.2 Clear enough a stranger could do it.
  - 6.1.3 **Testable** — you can prove done.
- 6.2 **Size limits are rules, not taste** (agents lose the thread when tasks bloat):
  - 6.2.1 *Warning:* >~4 code-changing checklist items, >~4 files, description >~150 words, or multi-layer span → consider split.
  - 6.2.2 *Must-split:* >~12 checklist items or >~10 files → no exceptions; split part-a / part-b with a clear boundary.
  - 6.2.3 Difficulty from signals, not gut: one file tiny change = easy; few files + new function + tests = medium; many files / new component / cross-cutting = hard (and hard hints “split”).
- 6.3 **Acceptance criteria:** yes/no only; no “better/cleaner.” Plus two globals stated once:
  - 6.3.1 **Global acceptance criteria** — e.g. automated checks pass; no new warnings; nothing existing breaks (scoped — see §12).
  - 6.3.2 **Prohibited outcomes** — tests that only check “didn’t crash”; error messages without cause; swallowed errors; single-use abstractions.
- 6.4 **Quality dimensions ride along** on each relevant task (copy from plan §3.9) so task 14 does not re-read the whole plan.
- 6.5 **Every named edge case → a test, one-for-one.**
  - 6.5.1 **Trap test:** at least one test a lazy/plausible-wrong solution fails.
  - 6.5.2 **Realistically shaped data** from real record definitions (ties to §5.6) — hand-waved fixtures can bless a wrong shape.
- 6.6 Every task record carries: unique ID, title, difficulty, **files/areas touched**, dependencies, optional pre-check / completion check / time budget / notes / needs-human flag.
- 6.7 **Foundations-first order:** data shapes & types → core logic → integrations → UI; basic behavior before extensions.
- 6.8 **Record facts; derive relationships.** Write *which files* a task touches; let the system derive “cannot run in parallel.” Hand-maintained dual lists drift. Only true ordering edges (“B needs A’s output”) are written as dependencies.
- 6.9 Task types and worker behavior:
  - 6.9.1 **Build** — one story + tests as one coherent change.
  - 6.9.2 **Contract** — *design only, no production code.* Interface, edges, invariants, failures, “wrong impl that would look right,” alternatives, dependent task list; full text in shared log. Changing a contract reopens the contract task — no quiet drift.
  - 6.9.3 **Review** — *look, don’t fix.* Judge against plan; spawn fix-ups; never implement fixes inline.
  - 6.9.4 **Fix-up** — must carry **root cause**, **exact fix**, **proof command** so a cheap model can land it in one pass.
  - 6.9.5 **Human-decision** — §11.
- 6.10 **Tasks say how hard, not which brain.** Difficulty only; **model routing** table is config (§10). Baked model names rot when vendors ship new IDs.
- 6.11 **Recall memory again at task-design time** with precise file/function names; paste into notes/AC.
- 6.12 Validate the finished list (Appendix E) before the first run.

> **Example — bad vs good task:** Bad: “Improve the list command.” Good: “TASK-012: Add `--status` filter… difficulty easy; touches list + tests; depends on record contract; AC: filter works, unknown status errors clearly, default unchanged, tests cover all three.”

> **Example — forced split:** “Add user accounts” with 15 items / 14 files → 020a storage, 020b sign-up/login, 020c password reset.

> **Canonical story — trap test:** “A reminder sent twice must be reported, not silently absorbed.” A fake that always returns “ok” fails.

> **Example — fix-up anatomy:** Root cause line 84 skip-with-no-message; exact fix collect + report line numbers; proof: bad-rows fixture lists lines 3 and 7.

---

## 7. The Standing Instruction Sheet & Context Thrift

> Promoted from task design: this artifact is as important as the task list.

- 7.1 Alongside the task list, produce **one short document the worker re-reads every round** — everything that applies to *every* task.
- 7.2 Contents:
  - 7.2.1 **Priority order when goals collide:** plan/anticipate edges → strong foundations → working code → provable correctness → clean code → polish.
  - 7.2.2 Global AC + prohibited outcomes (§6.3), distilled data-shape contracts (§5.6), and 5–10 one-line lessons from memory.
  - 7.2.3 Pointers into big docs (“see plan §edges,” not paste the whole plan).
- 7.3 **Context thrift:** agents have limited working memory per round. The sheet holds *distilled excerpts*; the worker fetches a specific section only when the current task points at it.
- 7.4 **Anti-pattern:** every round re-reads a 900-line plan, a whole team guidebook, and an ever-growing log — quality collapses before a line of code is written.

> **Field note — thrift:** With the sheet, three pages per round (rules, contracts, ten lessons, current task). Without it, most of the context window is gone on history.

---

## 8. Ordering the Work: The Dependency Graph

- 8.1 **DAG in plain words:** arrow from a task to what must finish first; **acyclic** = no loops.
- 8.2 Using the graph to check the plan:
  - 8.2.1 Loops = deadlock — find before start.
  - 8.2.2 Orphans (nothing needs them, they need nothing) often mistakes.
  - 8.2.3 Parallel candidates: no path between them **and** no shared files (from facts in §6.8).
- 8.3 **Hard vs soft dependencies:**
  - **Hard:** B cannot correctly start without A’s output (edge in the graph).
  - **Soft:** “nice if A lands first” (priority/scheduling hint only — must not freeze the run if A is delayed).
- 8.4 **Milestones:** gathering points where many arrows meet — natural full-gate and review spots (§12).
- 8.5 **Cross-project dependencies:** record “needs other project’s milestone X — reason …” explicitly; worker checks and raises **blocked** (§11) instead of inventing a private format.

> **Field note — review/fix-up deadlock:** Fix-up accidentally depends on the review that spawned it → freeze where everything looks “waiting.” Rule: no arrow from spawned fix back to its parent review.

> **Canonical story — parallel safety:** Send-path task and docs-only task share no files → parallel-safe; two tasks both touching reminder storage → serialized.

> **Example — cross-project:** Mobile app requires data-format project’s final milestone before reading the new record type.

---

## 9. The Work Loop: How the Agent Actually Runs

- 9.1 Basic loop: pick next ready task → read instruction sheet + task → do work → check results → **commit only non-broken work** (assumes a version-control workflow; if you don’t use git, “checkpoint a restore point”) → mark status → short journal entry → repeat. **One task per round.**
- 9.2 **One source of truth for runtime state:** task statuses live in one store; agents change them via **commands**, not freehand file edits of live state.
  - 9.2.1 Clarify: humans still *author* plans and initial task definitions; **runtime state** (in progress / done / blocked) is command-only so the loop and the files cannot split-brain.
- 9.3 **Status lifecycle:** each task in exactly one of: to-do, in progress, done, failed, skipped, blocked, no-longer-needed.
- 9.4 **Progress journal:** ~10 lines — approach in one sentence, files, 1–2 lessons. Future rounds read the *tail* or search one past task ID — never the whole log by default.
- 9.5 Parallel safely: different files only; each worker its own workspace copy; careful merge-back.
- 9.6 **Stopping criteria:** time/money/try budgets; clear finish lines. Before “project complete,” verify every task really done and no fresh fix-ups still open — not a vibe.

> **Field note — hand-edit split-brain:** Operator edits the live task file; loop re-imports its own view → tasks “vanish,” “not found.” Commands only for runtime state.

> **Canonical story — journal earning keep:** Later task greps “TASK-027” and gets “reminders in own table; times in UTC — convert at display” in seconds.

> **Field note — parallel workspace ghosts:** After merge, deleted worker copies leave cached builds pointing at dead paths → mass “file not found” that *looks* like a logic regression. Rebuild before trusting those failures; record as learning (§14).

> **Example — budget:** ≤3 tries and 30 minutes per task; whole run stops after 4 hours or when every task is done/failed/blocked.

---

## 10. Right-Sizing the Brain: Matching Model Power to Difficulty

- 10.1 Not every task needs the strongest (most expensive) model.
- 10.2 **Model routing:** config table maps difficulty → capability. Tasks carry difficulty only (§6.10). Models change → update one table.
- 10.3 Standing upgrades: reviews and final gates default to the strongest model — a weak reviewer waves through exactly what the review exists to catch.
- 10.4 **Escalation ladder:** on failure, stronger model before give-up (record promotion so the run does not ping-pong).
- 10.5 **Fallbacks:** provider down → same capability tier on another provider when configured; distinguish **ephemeral blackout** from permanent route changes.
- 10.6 **Where the money goes (cost model, short):** tokens spend on (a) interview rounds, (b) worker tasks, (c) review passes, (d) full milestone gates, (e) retries/escalation. Routing optimizes (b); thrift (§7) optimizes all of them. Ceremony that prevents a wrong multi-hour feature is cheap; ceremony on a 15-minute chat is expensive (§17).

> **Example — routing:** Typo in help → cheap model. Race on double-claim → top model + thinking. Wrong mapping wastes money *or* tries.

> **Anti-pattern — model names in tasks:** 60 tasks say “use model X-2”; X-3 ships; every list is wrong. Difficulty + one routing table survives.

> **Field note — provider launch path down for two days:** With same-tier fallback, run slows but continues; without, every task blocks.

---

## 11. Humans in the Loop: When the Agent Should Wake You

- 11.1 People own: security trade-offs, spend limits, hard-to-undo choices, and risky open-ended research (§2). Flag those tasks up front.
- 11.2 **Clarification / human-decision tasks:** marked needs-human; agent **blocks** instead of guessing.
- 11.3 While blocked on one task, the agent may continue **other ready tasks** that do not depend on the answer (graph-aware pause, not full freeze).
- 11.4 Record the human answer in **machine-readable form** and update every downstream task that embedded the *proposed* value in the **same change** — or workers faithfully build the wrong number.
- 11.5 Partial answers and timeouts: if the human answers only part of the question, re-open or re-block with the remainder; if no answer within the run’s human SLA, leave blocked with a clear reason — do not invent.
- 11.6 **Blocked signal:** standard “I’m stuck and why,” never quiet wrong work.
- 11.7 Interview (§4) and clarification tasks are the same idea at two moments: before planning vs during the run.
- 11.8 **Multi-human ownership (lightweight):** name who owns the plan, who may resolve CLARIFY tasks, who merges parallel work. Unowned human gates are how overnight runs stall until Monday with no alert path.

> **Canonical story — rate / send limits:** Proposed “100/min” or “100/day” affects billing → needs-human. Agent blocks, works elsewhere. Human chooses 60; machine-readable outcome + all downstream “100”s updated together.

> **Anti-pattern:** Human decides in chat only; tasks still say the proposed value → agent implements the proposal.

---

## 12. Checking the Work: Verification and Quality Gates

- 12.1 Trust but verify: “done” is a claim; tests and gates are proof.
- 12.2 **Quality gates:** tests, formatters, linters, type checkers that must pass before done.
- 12.3 **Two speeds:**
  - 12.3.1 Per task: fast **scoped** gate (format, types, lint, tests near touched files).
  - 12.3.2 Per milestone: **full** project gate.
  - 12.3.3 **Policy choice — pre-existing failures at milestones:**  
    - *Strict:* milestone leaves trunk green including old failures (stops rot; can stall the run).  
    - *Pragmatic:* **known-fail allowlist** (named tests + owner + ticket) may remain red; everything *new* or *touched* must be green; unlisted failures are not “not my mess.”  
    Pick one policy per team and write it on the instruction sheet.
  - 12.3.4 Escape hatch for a large unrelated red pile: fix what this project caused; file **one** task listing the rest; block for human ownership routing — no silent skip, no week-long heroics on someone else’s backlog.
- 12.4 Test-first habit for bugs: write failing test → fix → pass.
- 12.5 **Review passes:** separate from build. Especially: **is new code wired into the real production path?** Green unit tests with doubles can hide unreachable features (never registered, setting read but not passed, function never called).
- 12.6 Fix-ups re-enter the graph with correct arrows + root-cause / exact-fix / proof anatomy (§6.9.4).

> **Canonical story — two-speed + false done:** Scoped gate ~90s on import-adjacent work; milestone full suite finds unrelated export break + (policy-dependent) old red. Gate flips false “done” back to in-progress.

> **Canonical story — built but never plugged in:** Send function tested in isolation; scheduler never registers it. Review spawns wiring fix-up with proof: “trigger produces a real send attempt / file.”

---

## 13. Handling Failure: Because Things Will Go Wrong

- 13.1 Expect failure; respond in order: **retry → escalate → reroute → block → ask a human** (see Appendix F).
- 13.2 **Taxonomy matters:** flaky network ≠ bad credentials ≠ real product bug. Wrong bucket punishes healthy tasks for weather.
- 13.3 **Protect shared state:** snapshot orchestrator-owned files before risky agent steps; restore if corrupted.
- 13.4 **Guardrails in tools, not memos:** e.g. allow untrack, refuse physical delete — rules tools enforce cannot be forgotten on a bad day.
- 13.5 **Security & trust boundary (short but first-class):**
  - Secrets never in plans, tasks, journals, or prompts committed to the repo.
  - Destructive ops (drop DB, force-push, mass delete) require human-decision tasks or hard tool denial.
  - Treat plan/task text as **untrusted input to the worker** — a poisoned plan is a prompt-injection path; only trusted humans/process edit the plan SSoT.
  - Least privilege for agent credentials in overnight runs.

> **Example table (draft as small table):**  
> | Failure | Response | Count against task? |  
> | Provider 502 | wait + retry | no |  
> | Invalid credentials | pause run, alert human | no |  
> | Same product test fail ×2 | escalate model or block with note | yes |

> **Field note — snapshot/restore:** Agent “cleans up” settings; post-check restores snapshot; corruption does not spread.

> **Field note — delete vs untrack:** Agent deleted a file from disk when asked to untrack; fix was a hard tool rule, not a nicer prompt.

---

## 14. Memory: Learning From Every Project

- 14.1 **Recorded learnings:** after success, failure, or workaround — 1–2 lines: what, why, how to apply. Short notes get read.
- 14.2 **Recall** before plan (§3.5), task design (§6.11), and each task start: match files, task types, error strings; dual search (tags + free text).
- 14.3 **Memory hygiene:** retire stale, merge duplicates, track which notes helped (so good ones surface).
- 14.4 **Anti-pattern — polluted memory:** a wrong lesson, repeated confidently, trains every future run. Invalidation/degrade path is part of the design, not optional.
- 14.5 **Decision logs:** architectural choices + rejected options so future work does not relitigate.

> **Field note — ghosts learning pays off:** “Mass failures naming a deleted workspace folder → rebuild first.” Later run matches the error, rebuilds in minutes.

> **Example — decision log:** One shared DB per project, not per-feature files (merge conflicts under parallel workers); not cloud-only (must work offline).

---

## 15. Putting It All Together: The Full Cycle

- 15.1 End-to-end flow:
  1. Explore (spike + timebox + fallback)
  2. Plan (what/why: goals, quality, named edges, approaches, risks, deliberate cuts)
  3. Interview (fork; write answers back)
  4. Contracts (connectors + verified shapes)
  5. Tasks + instruction sheet (size limits, trap tests, context thrift)
  6. Graph (hard edges, milestones, parallel facts)
  7. Run (loop, routing, humans, journal)
  8. Verify (scoped + milestone policy, wiring reviews)
  9. Capture learnings
- 15.2 **One worked story — “add email reminders”** (assemble all *Canonical story* snippets here only; earlier sections only teaser).
- 15.3 **Health signals** (expand; metrics detail in §18):
  - Tasks rarely bounce (done → reopened)
  - Humans asked early, not after wrong implementation
  - Escalations rare and one-way
  - Each project’s plan starts with prior scars already priced in
  - Milestone red is explained (new vs allowlisted)

> **Canonical story — full walkthrough (email reminders):**  
> (1) Spike: one real send; save success/failure; timebox one day.  
> (2) Plan: 24h before due; never double-send; no SMS; cut custom sounds; stop-sign on daily cap → batching chosen.  
> (3) Interview: timezone; edit-due-date-after-queue; write-back.  
> (4) Contract: reminder record + verified read.  
> (5) Six small tasks + trap test on double-send; send-time task needs-human; sheet carries priority order + contract + five lessons.  
> (6) Graph: all depend on contract; two parallel-safe modules.  
> (7) Run: easy → cheap model; scheduling → strong; block on human; journal ~10 lines.  
> (8) Verify: scoped gates; milestone catches duplicate-send; review finds unregistered send function → wiring fix-up.  
> (9) Learn: “provider silently drops >1 MB bodies” → next project’s spike checks day one.

---

## 16. Why This Design Pays Off: Cross-Cutting Benefits

> Place **after** the full cycle so the method is complete before system effects are sold. Each piece solved a local problem; together they produce effects no single checklist item owns.

- 16.1 **The graph buys safety and speed together.** Arrows enforce foundations-first; *absence* of arrows + file facts shows safe parallel work. Order and speed from one picture.
- 16.2 **A fresh mind for every task (thesis).** Each task starts a **new conversation** — no fifty-task chat sludge. Cheaper (fewer tokens) and sharper (full attention on *this* task). The plan, instruction sheet, and journal **replace conversation history as memory** — which is why they must be written down. This is **externalized memory over chat history.**
- 16.3 **The run gets smarter mid-flight.** A lesson after task 5 injects into task 6 the same day — not at the next retrospective.
- 16.4 **Searchable “how we got here.”** Short structured journal prevents thrashing: redoing work, undoing deliberate decisions, re-solving solved problems.
- 16.5 **Insights compound into easier planning.** Run notes feed the next project’s memory check (§3.5, §14). Planning gets faster and sharper instead of starting from zero.
- 16.6 **Health is scheduled, not hoped for.** Reviews, tests, and cleanup are *tasks in the graph* with IDs and arrows — they cannot be “when we get time.”

> **Example — benefits stacking (keep one composite story):** Mid-run task 9 learns test DB resets; tasks 10–20 avoid it; 11∥12 parallel on graph; task 15 journal-searches table split; forced review catches unwired function; *next* project’s plan already carries the reset warning.

---

## 17. When to Use This, When to Skip It, and How to Adopt

### 17.1 Decision table — chat vs light tasks vs full cycle

| Situation | Use |
|-----------|-----|
| Single concern, <~15–30 min, you can watch, “done” is obvious | **Conversation** with your coding agent. No PRD, no graph. |
| A few files, clear AC, still interactive, maybe 1–5 checkable steps | **Light task list** (titles + AC only). Skip spike/interview/contracts unless a shared connector appears. |
| Multi-hour / overnight, many files, ambiguous done, dependencies, or you will be offline | **Full cycle** (this paper). |
| Open-ended research with no product change yet | **Spike only** (§2). Do not fake a 20-task plan around an untested hypothesis. |
| Pure ops / one-off script you will throw away | Conversation or light list; don’t build institutional memory theater. |

- 17.2 **Rule of thumb:** If explaining “done” takes longer than doing the work, you over-ceremonied. If the agent has already declared done twice and you still can’t list leftovers, you under-ceremonied.
- 17.3 **Adoption path**
  - **Week 1:** One medium project (~10–20 tasks). Plan + tasks + scoped gates only. No parallel, no multi-provider.
  - **First full PRD-scale effort:** Add interview write-back, one contract, one milestone full gate, human-decision for one real policy number.
  - **First multi-day / overnight run:** Add budgets, blocked signal + continue-other-work, journal discipline, model routing table.
  - **Ongoing:** Memory recall into plans; decision log; memory hygiene; optional parallel when file facts are trustworthy.
- 17.4 **Team roles to name early:** plan owner, CLARIFY resolver, merge owner (can be one person on a small team — but *named*).

> **Example — good first project:** Export feature on an internal tool, ~10–20 tasks. Big enough for a contract and a review; small enough a mistake costs an afternoon.

---

## 18. Measurement: Is the *Workflow* Working?

> Success of the product feature ≠ success of the methodology. Measure both.

- 18.1 **Suggested metrics** (start with 3–4, not all):
  - **First-pass done rate** — % tasks done without reopen/fix-up.
  - **Bounce rate** — done → in progress / failed again.
  - **Human interrupts per completed task** — and % of those that were *flagged in advance* vs surprise.
  - **Time / tokens per done task** (and per milestone).
  - **Escalation rate** — fraction of tasks that climbed the model ladder.
  - **Parallel efficiency** — wall clock vs sum of sequential estimates when parallel used.
  - **Plan stability** — how often mid-run plan edits were required (high can mean weak interview).
  - **Memory hit rate** — tasks where a recalled learning changed behavior (survey or log tag).
- 18.2 **Healthy ranges are team-local.** Direction matters: bounces down, early humans up (as share of interrupts), tokens per task down after thrift, plan edits front-loaded in interview not day 4.
- 18.3 **What not to optimize:** raw tasks closed per day (encourages tiny meaningless tasks); zero human interrupts (encourages guessing).

---

## 19. Conclusion

- 19.1 **One-sentence thesis:** Coding agents do their best long-running work when a human designs the *system* — clear goals, small checkable steps, honest states, scheduled human gates, and memory outside the chat — and the agent executes the steps under **bounded autonomy**.
- 19.2 **Three non-negotiables** (if you remember nothing else):
  1. **Written plan as source of truth** — interview decisions must land in the plan; chat is not memory.
  2. **Small checkable tasks** — size limits, acceptance criteria, trap tests; one task per round.
  3. **Gates + memory** — scoped/milestone verification and short learnings so the next hour (and next project) is smarter than freelancing.
- 19.3 **What failure looks like when you skip pieces:**
  - Interview without write-back → faithful wrong implementation.
  - Reviews that implement fixes inline → no graph, no proof, scope creep.
  - No trap tests → green suite, fake solutions.
  - Model names baked into tasks → rot on every vendor release.
  - Full ceremony on a 15-minute job → process theater; agents look slow; humans abandon the method.
- 19.4 **First experiment this week:** Pick one real, forgiving project (~10–20 tasks). Write a short plan, run a 3-question interview and write answers back, split into sized tasks with AC, run with scoped gates and a one-page instruction sheet. Compare to your last “big chat that went sideways.”
- 19.5 Start small; expand only when the pain of *not* having the piece shows up (missing contract, surprise human decision, unmeasured overnight mess).

---

## Appendices

- **A. Glossary** — plain-language definitions of every bolded concept (coding agent, workflow, bounded autonomy, hypothesis, contract, invariant, acceptance criteria, trap test, DAG, hard/soft dependency, milestone, model routing, escalation ladder, blocked signal, quality gate, scoped vs full gate, known-fail allowlist, context thrift, standing instruction sheet, progress journal, learning, decision log, etc.).
- **B. One-page task template** — ID, title, difficulty, files touched, dependencies, AC, pre-check, completion check, time budget, notes, needs-human — filled sample: TASK-012 from §6.
- **C. Starter interview questions** — fuzzy words, limits, conflicts, hard-to-undo, “what happens when…?” — seed list for §4, not a script.
- **D. Plan completeness checklist** — quality dimensions; ≥2 named edges; ≥1 rejected approach; top 3 risks; consumers of changed behavior; deliberate cuts; documentation plan; every section filled or “none — on purpose.”
- **E. Task-list + graph checklist** — every story represented; every edge case → test; trap test per task; size limits; no model names on tasks; research/human flags; instruction sheet complete; no loops; no bad orphans; fix-up arrows correct; parallel share no files; cross-project links with reasons; hard vs soft deps not confused.
- **F. Sample failure-response ladder** — retry → escalate → reroute → block → human; annotated with §13 failure stories.
- **G. When-to-skip matrix** — expanded version of §17.1 with examples per cell (chat / light / full / spike-only).
- **H. Metrics starter sheet** — definitions, how to log lightly, and “healthy direction” notes from §18.
- **I. Related work & positioning (short)** — classical project management & WBS; ticket systems (Jira et al.) without agent loops; free-roaming agents (AutoGPT-style) without durable plans; “spec-driven” / design-doc culture; where this methodology sits: *spec + executable task graph + externalized memory + bounded agent loop for coding.* Not a literature review — a orientation map so readers are not sold “we invented planning.”

---

## Named field notes / stories index (for cross-reference while drafting)

| Name | Primary sections |
|------|------------------|
| Billing feature “done!” too early | §1 |
| Email reminders (canonical end-to-end) | §2–§3, §4–§6, §8–§12, §15 |
| Launch-path / provider CLI surprise | §2, §10 |
| Café search edge | §3 |
| Date format dual purpose | §3 |
| Interview without write-back | §4 |
| Five shapes of task record | §5 |
| Silent nested options | §5 |
| Review/fix-up deadlock | §8 |
| Hand-edit split-brain | §9 |
| Parallel workspace ghosts | §9, §14 |
| Built but never plugged in | §12 |
| Snapshot restore / delete-vs-untrack | §13 |

---

## Drafting order suggestion

1. §1 + §17 (audience and scope lock the voice).  
2. §15 canonical story (forces consistency of examples).  
3. §3–§7 (plan → tasks → sheet).  
4. §9–§13 (runtime).  
5. §16, §18–§19, appendices.  
6. Figure (map) and glossary pass last.
