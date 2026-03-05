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
