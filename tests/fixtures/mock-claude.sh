#!/bin/bash
# Mock Claude script for E2E loop integration testing.
#
# Simulates a Claude Code session that completes the assigned task.
# Called with: mock-claude.sh --print --dangerously-skip-permissions -p <PROMPT>
#
# Environment variables:
#   TASK_MGR_BIN  - Path to the task-mgr binary
#   TASK_MGR_DIR  - Path to the project directory (for task-mgr --dir)
#   MOCK_RUN_ID   - Current run ID (for task-mgr complete --run-id)
#
# The script extracts the task ID from the prompt JSON, marks it complete
# via task-mgr, then outputs <promise>COMPLETE</promise>.

set -euo pipefail

# Parse the -p flag to get the prompt content
PROMPT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -p)
            PROMPT="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

if [[ -z "$PROMPT" ]]; then
    echo "Error: No prompt provided via -p flag" >&2
    exit 1
fi

# Extract task ID from the prompt JSON block
# The prompt contains: "id": "LOOP-001" (or similar) in the Current Task section
TASK_ID=$(echo "$PROMPT" | grep -oP '"id"\s*:\s*"([^"]+)"' | head -1 | grep -oP '"[^"]+"\s*$' | tr -d '"' | tr -d ' ')

if [[ -z "$TASK_ID" ]]; then
    echo "Error: Could not extract task ID from prompt" >&2
    exit 1
fi

# Complete the task using task-mgr
if [[ -n "${TASK_MGR_BIN:-}" ]] && [[ -n "${TASK_MGR_DIR:-}" ]]; then
    "$TASK_MGR_BIN" --dir "$TASK_MGR_DIR" complete "$TASK_ID" \
        ${MOCK_RUN_ID:+--run-id "$MOCK_RUN_ID"} \
        --force 2>/dev/null || true
fi

# Output completion signals that the detection engine expects
echo "Working on task: $TASK_ID"
echo "Task $TASK_ID completed successfully."
echo "<promise>COMPLETE</promise>"
exit 0
