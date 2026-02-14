#!/bin/bash
# Mock Claude script for integration testing.
# Outputs predictable responses based on the input prompt.
#
# Usage: mock_claude.sh [--complete|--blocked|--error]
#
# Options:
#   --complete  Output a successful completion marker
#   --blocked   Output a blocked marker (simulates blocker)
#   --error     Output an error message (non-zero exit)
#   (default)   Output a simple success message
#
# The script reads stdin but ignores it - the mode is controlled by flags.

set -e

MODE="success"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --complete)
            MODE="complete"
            shift
            ;;
        --blocked)
            MODE="blocked"
            shift
            ;;
        --error)
            MODE="error"
            shift
            ;;
        *)
            shift
            ;;
    esac
done

# Drain stdin to prevent broken pipe
cat > /dev/null 2>&1 || true

case "$MODE" in
    complete)
        echo "Task completed successfully."
        echo "<promise>COMPLETE</promise>"
        exit 0
        ;;
    blocked)
        echo "Encountered a blocker that prevents progress."
        echo "<promise>BLOCKED</promise>"
        exit 0
        ;;
    error)
        echo "Error: Simulated failure" >&2
        exit 1
        ;;
    *)
        echo "Mock Claude: Task processed successfully."
        exit 0
        ;;
esac
