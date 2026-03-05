❯ For claude loop Would it be possible to select a different model based on the task or the task difficulty?

● Yes, definitely feasible. The claude CLI accepts a --model flag, so it's a matter of where the model selection lives. A few design options:

Option A: Per-task field in the PRD JSON
{
"id": "TASK-001",
"title": "Write fixture test",
"model": "claude-haiku-4-5-20251001"
}
The loop reads the next task's model field and passes --model to claude. Fine-grained but requires editing every task.

Option B: Difficulty mapping in PRD
{
"id": "TASK-001",
"difficulty": "low"
}
The PRD or loop config maps low → haiku, medium → sonnet, high → opus. You only set difficulty, not a model name.

Option C: CLI flag as default + per-task override
./claude-loop.sh --model claude-haiku-4-5-20251001 tasks/foo.json tasks/foo-prompt.md
A --model flag sets the default for the whole run; individual tasks can still override via their JSON field.

Option D: Top-level PRD default
{
"model": "claude-haiku-4-5-20251001",
"userStories": [...]
}
One model for the whole PRD, no per-task granularity.

---

My recommendation would be C + A together: CLI flag sets the run default, per-task model field overrides it for specific tasks. That covers both "run cheap tasks with haiku" and "this one
tricky task needs opus" without forcing you to annotate every task.

Want me to implement that, or do you prefer a different approach?

❯ I like A + D actually. Set a default on the PRD, with tasks overriding it. I would like to add one twist which is all tasks with a high difficulty level should be run with Opus unless the
task overrides it. So if a PRD has Haiku is the default, a task with high level of difficulty would use opus. A task with a medium level would still use Haiku, and a task with medium
that has an override 'model' key with a value of sonnet would use Sonnet. A task with a high level of difficulty, and an override for sonnet would also use sonnet.

❯ There's one concern which is that each iteration may often complete more that one task at a time even though the prompt says to only complete one. So if a task that would normally use
Haiku has synergy or is batched with a a task that needs Opus, it should probably use Opus.

● Good point. Since we can't predict which tasks get batched together in a single iteration, here are two practical approaches:

Simple: Scan all pending tasks for the iteration. Use the highest model tier among them. Once the hard tasks complete, subsequent iterations naturally drop down to the cheaper model.

Smarter: Look at pending tasks whose touchesFiles overlap with the first task (the same synergy logic the prompt uses for task selection). Use the highest model in that cluster.

The simple approach might burn a few extra opus iterations on easy tasks, but it's predictable and avoids the scenario you described. The smarter approach is more precise but adds complexity
and could miss batching that happens for other reasons.

Which do you prefer?

❯ Go with the smarter approach. Add a dynamic section to the prompt that shows up when using a lower model, and it should state that if there are many issues trying to meet the task
acceptance criteria, it should stop the current effort, make a note in the task and update the diffiulty to 'high', revert the code to the last commit, and then end that iteration.

# Inject escalation instructions when running a non-opus model

ESCALATION_CONTEXT=""
if [ -n "$ITERATION_MODEL" ] && [["$ITERATION_MODEL" != *opus*]]; then
ESCALATION_CONTEXT="## Model Escalation Policy
You are running as a **cost-optimized model** for this iteration. If you encounter
significant difficulty meeting the task's acceptance criteria — for example, repeated
test failures, architectural complexity beyond your confidence level, or you find
yourself going in circles — follow this escalation procedure:

1. **Stop** your current implementation effort immediately.
2. **Revert** only the files you changed during this iteration: run \`git diff --name-only | xargs git checkout --\`
3. **Update the task** in the PRD JSON file:
   - Set \`\"difficulty\": \"high\"\` on the task object.
   - Add a \`\"escalationNote\"\` field with a brief explanation of what went wrong
     and what approach you attempted (this helps the next iteration).
4. **End this iteration** — do not attempt the task again.

The next iteration will automatically use a more capable model for high-difficulty tasks.
Do NOT set difficulty to high preemptively — only escalate after a genuine failed attempt.

---

"
echo " [Escalation policy injected (non-opus model)]"
fi

❯ THINK what is the right way to handle this scenario? Goal is to
make sure we can restart a session when appropriate, and start
with a clean new session when new work is starting.

● Let me think through the scenarios:

Scenario: Crash mid-task, retry within same run
Right behavior: Resume
Why: Session has context about in-progress work
────────────────────────────────────────
Scenario: External kill, restart, same task still pending
Right behavior: Resume
Why: Same situation as above, just different restart path
────────────────────────────────────────
Scenario: Task completed, moving to next task
Right behavior: Fresh
Why: Previous task's context is noise for the new task
────────────────────────────────────────
Scenario: All tasks done, user runs loop again later
Right behavior: Fresh
Why: Stale context
────────────────────────────────────────
Scenario: User manually fixed things between runs
Right behavior: Fresh
Why: Session context may contradict manual changes

The discriminator is: is the same task still pending?

So the session file should store both the session ID and the task ID
it was working on. On each iteration:

- If .resume-session exists AND its task ID matches the current
  NEXT_TASK → resume
- Otherwise → fresh session

This is one line change to the file format (SESSION_UUID TASK_ID)
and a simple comparison. No new flags, no CRASH_COUNT gating,
handles every scenario above correctly.

● Clean. Now let me also check the old .resume-session file if it
exists — it has the old single-value format (just a UUID or
timestamp). The awk '{print $2}' will return empty for those, so
SAVED_TASK will be empty and it won't match NEXT_TASK, correctly
falling through to a fresh session. Backward-compatible.

Let me verify the crash handler no longer touches the file (from our
earlier fix):

● Read 1 file (ctrl+o to expand)

● Good — the crash handler leaves the file alone (from our earlier
fix). The SESSION_UUID TASK_ID pair written at iteration start
persists through crashes.

Here's a summary of the behavior:

Scenario: Fresh start, no file
.resume-session: absent
Result: New UUID, new session
────────────────────────────────────────
Scenario: Crash on TASK-X, retry
.resume-session: uuid1 TASK-X
Result: Resume uuid1, model escalated one tier
────────────────────────────────────────
Scenario: Task-X done, next is TASK-Y
.resume-session: uuid1 TASK-X
Result: Stale — discarded, fresh session for TASK-Y
────────────────────────────────────────
Scenario: External kill, restart, same task
.resume-session: uuid1 TASK-X
Result: Resume uuid1, model escalated
────────────────────────────────────────
Scenario: Old-format file from before this change
.resume-session: uuid1 (no task)
Result: SAVED_TASK is empty, won't match → fresh session
