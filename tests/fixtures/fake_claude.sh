#!/bin/bash
# Fake claude binary for integration tests.
#
# Reads the prompt from stdin (matching real claude behaviour when invoked with
# bare -p flag and prompt piped via stdin), extracts "ID: N" lines, and outputs
# a valid enrich JSON response with predetermined metadata for each learning ID.
#
# Usage (mirrors real claude CLI):
#   echo "<prompt>" | fake_claude.sh --print --dangerously-skip-permissions -p

# Read the entire prompt from stdin
prompt="$(cat)"

# Extract IDs from the prompt (format: "ID: N" on its own line after "---")
output="["
first=true
while IFS= read -r line; do
    if [[ "$line" =~ ^ID:\ ([0-9]+)$ ]]; then
        id="${BASH_REMATCH[1]}"
        if $first; then
            first=false
        else
            output="$output,"
        fi
        output="$output{\"learning_id\":$id,\"applies_to_files\":[\"src/**/*.rs\"],\"applies_to_task_types\":[\"FEAT-\",\"FIX-\"],\"applies_to_errors\":[\"E0308\"],\"tags\":[\"rust\",\"testing\"]}"
    fi
done <<< "$prompt"
output="$output]"
echo "$output"
