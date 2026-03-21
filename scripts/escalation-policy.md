## Stuck Loop Detection and Auto-Block

The loop engine tracks consecutive failures per task. When a task fails
`maxRetries` times in a row (default: 3), the engine **auto-blocks** the task:

- `consecutive_failures` increments on each failed or incomplete iteration
- When `consecutive_failures >= max_retries`, the task status is set to `"blocked"`
- Blocked tasks are excluded from selection — the loop moves on to other work
- The loop terminates with `NoEligibleTasks` when all remaining tasks are blocked
  or have unmet dependencies

### What counts as a failure?

An iteration counts as a failure (increments `consecutive_failures`) when:
- The agent completes without outputting `<completed>TASK-ID</completed>`
- The agent outputs `<promise>BLOCKED</promise>`
- The loop engine detects no new commits after the iteration

A successful completion resets `consecutive_failures` to 0 for that task.

### Preventing false blocks

To avoid auto-blocking on recoverable errors:
- Output `<promise>BLOCKED</promise>` only for genuine external blockers
  (missing dependency, ambiguous requirements, infrastructure unavailable)
- Do NOT output `BLOCKED` for recoverable implementation errors — fix and commit instead
- If a task is partially done, commit the partial work before stopping;
  the next iteration can continue where you left off

### Per-task retry limits

Tasks can override the global `defaultMaxRetries` with a `maxRetries` field:
```json
{
  "id": "FEAT-007",
  "maxRetries": 5,
  "description": "Complex migration — allow extra attempts"
}
```

---

## Model Escalation Policy

You are running as a **cost-optimized model** for this iteration. If you encounter
significant difficulty meeting the task's acceptance criteria -- for example, repeated
test failures, architectural complexity beyond your confidence level, or you find
yourself going in circles -- follow this escalation procedure:

1. **Stop** your current implementation effort immediately.
2. **Revert** only the files you changed during this iteration:
   run `git diff --name-only | xargs git checkout --`
3. **Update the task** in the PRD JSON file:
   - Set `"difficulty": "high"` on the task object.
   - Add an `"escalationNote"` field with a brief explanation of what went wrong
     and what approach you attempted (this helps the next iteration).
4. **End this iteration** -- do not attempt the task again.

The next iteration will automatically use a more capable model for high-difficulty tasks.
Do NOT set difficulty to high preemptively -- only escalate after a genuine failed attempt.

### When to escalate vs. when to try harder

**Escalate when:**
- You've attempted the same approach twice and hit the same failure
- The architectural complexity exceeds your confidence level
- You're circling without progress (writing → reverting → writing)

**Do NOT escalate for:**
- First-attempt compilation errors (fix and retry)
- Linter warnings (apply suggestions and recommit)
- Test failures with a clear root cause (debug and fix)
