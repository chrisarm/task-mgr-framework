# /review-plan

When this command is invoked with a task description:

1. Enter plan mode and draft an implementation plan
2. Spawn the `production-code-architect` agent via Task tool
3. Have the architect review the plan and identify:
   - Architectural concerns
   - Missing requirements
   - Clarifying questions
4. Present me with:
   - The revised plan
   - Any questions from the architect
   - Trade-offs identified
5. Wait for my feedback before proceeding

Task: $ARGUMENTS
