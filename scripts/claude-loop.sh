#!/bin/bash
# DEPRECATED: Superseded by Rust loop engine (src/loop_engine/). Kept as reference implementation.
# Claude Code Loop with task-mgr integration
#
# Usage: ./claude-loop.sh [max_iterations] [prd_file] [prompt_file]
#
# This script runs Claude Code in a loop, each iteration working on
# the next recommended task from task-mgr. The task-mgr CLI handles
# task selection, progress tracking, and recovery.
#
# Arguments:
#   max_iterations  Maximum loop iterations (default: 10)
#   prd_file        JSON PRD file path (required for init)
#   prompt_file     Prompt markdown file (default: scripts/prompt.md)
#
# Options:
#   -y, --yes       Auto-confirm all prompts (for non-interactive use)
#   --dir DIR       task-mgr database directory (default: .task-mgr)
#
# Steering:
#   steering.md     - Edit this file to inject guidance into the next iteration
#   .pause          - Touch this file to pause and enter interactive guidance
#   .stop           - Touch this file to gracefully stop after current iteration
#
# SECURITY NOTES:
#   PRD files are considered trusted input. The JSON content from task-mgr output
#   (task descriptions, notes, learnings) is embedded into the prompt passed to
#   claude. This is safe because:
#   1. The JSON is produced by task-mgr from user-provided PRD files
#   2. jq -c produces properly escaped JSON strings
#   3. The prompt is passed as a single string argument to claude (not eval'd)
#   4. Claude treats the content as text, not executable commands
#
#   DO NOT modify this script to:
#   - Use eval on any JSON content
#   - Execute JSON values as shell commands via backticks or $()
#   - Pass JSON content through command substitution

set -e

# ============================================================================
# Configuration & Defaults
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Default task-mgr database directory
TASK_MGR_DIR="${TASK_MGR_DIR:-.task-mgr}"

# ============================================================================
# Argument Parsing
# ============================================================================

AUTO_CONFIRM=false
POSITIONAL_ARGS=()

while [[ $# -gt 0 ]]; do
  case $1 in
    -y|--yes)
      AUTO_CONFIRM=true
      shift
      ;;
    --dir)
      TASK_MGR_DIR="$2"
      shift 2
      ;;
    *)
      POSITIONAL_ARGS+=("$1")
      shift
      ;;
  esac
done

set -- "${POSITIONAL_ARGS[@]}"

MAX_ITERATIONS=${1:-10}
PRD_FILE=${2:-""}
PROMPT_FILE=${3:-"$SCRIPT_DIR/prompt.md"}

# Validate MAX_ITERATIONS is a positive integer
if ! [[ "$MAX_ITERATIONS" =~ ^[0-9]+$ ]] || [ "$MAX_ITERATIONS" -lt 1 ]; then
  echo "Error: MAX_ITERATIONS must be a positive integer, got: '$MAX_ITERATIONS'"
  exit 1
fi

# Resolve paths to absolute if needed
if [[ -n "$PRD_FILE" && "$PRD_FILE" != /* ]]; then
  PRD_FILE="$(pwd)/$PRD_FILE"
fi
if [[ "$PROMPT_FILE" != /* ]]; then
  PROMPT_FILE="$(pwd)/$PROMPT_FILE"
fi
if [[ "$TASK_MGR_DIR" != /* ]]; then
  TASK_MGR_DIR="$(pwd)/$TASK_MGR_DIR"
fi

# ============================================================================
# Dependency Checks
# ============================================================================

check_dependencies() {
  local missing=()

  for cmd in jq git task-mgr; do
    if ! command -v "$cmd" &> /dev/null; then
      missing+=("$cmd")
    fi
  done

  if [ ${#missing[@]} -gt 0 ]; then
    echo "Error: Required commands not found: ${missing[*]}"
    echo ""
    echo "Install task-mgr with: cargo install --path /path/to/task-mgr"
    exit 1
  fi
}

check_dependencies

# ============================================================================
# Load Environment
# ============================================================================

if [ -f "$PROJECT_DIR/.env" ]; then
  set -a
  # shellcheck source=/dev/null
  source "$PROJECT_DIR/.env"
  set +a
fi

# ============================================================================
# Steering Files
# ============================================================================

STEERING_FILE="$TASK_MGR_DIR/steering.md"
PAUSE_FILE="$TASK_MGR_DIR/.pause"
STOP_FILE="$TASK_MGR_DIR/.stop"

SESSION_GUIDANCE=""
GRACEFUL_STOP=false

# ============================================================================
# Run State
# ============================================================================

RUN_ID=""
CURRENT_TASK_ID=""
LAST_FILES=""

# ============================================================================
# Cleanup Handler
# ============================================================================

cleanup() {
  local exit_code=$?

  echo ""
  echo "Cleaning up..."

  # Export current state to JSON for crash recovery
  if [ -n "$PRD_FILE" ]; then
    echo "Exporting state to $PRD_FILE..."
    task-mgr --dir "$TASK_MGR_DIR" export --to-json "$PRD_FILE" 2>/dev/null || true
  fi

  # End the run if we started one
  if [ -n "$RUN_ID" ]; then
    local status="aborted"
    if [ "$GRACEFUL_STOP" = true ] || [ $exit_code -eq 0 ]; then
      status="completed"
    fi
    echo "Ending run $RUN_ID with status: $status"
    task-mgr --dir "$TASK_MGR_DIR" run end --run-id "$RUN_ID" --status "$status" 2>/dev/null || true
  fi

  # Clean up temp file
  [ -f "$OUTPUT_FILE" ] && rm -f "$OUTPUT_FILE"

  exit $exit_code
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ============================================================================
# Helper Functions
# ============================================================================

# Initialize or validate database
initialize_database() {
  local db_file="$TASK_MGR_DIR/tasks.db"

  if [ -f "$db_file" ]; then
    echo "Database exists, running doctor check..."
    task-mgr --dir "$TASK_MGR_DIR" doctor --auto-fix || {
      echo "Warning: doctor found issues, attempting recovery..."
    }

    # If PRD file provided, check if we need to sync
    if [ -n "$PRD_FILE" ]; then
      echo "Syncing database with PRD file..."
      task-mgr --dir "$TASK_MGR_DIR" init --from-json "$PRD_FILE" --append --update-existing || {
        echo "Error: Failed to sync with PRD file"
        exit 1
      }
    fi
  else
    # Fresh initialization required
    if [ -z "$PRD_FILE" ]; then
      echo "Error: No existing database and no PRD file specified"
      echo "Usage: $0 [iterations] <prd_file> [prompt_file]"
      exit 1
    fi

    echo "Initializing database from $PRD_FILE..."
    mkdir -p "$TASK_MGR_DIR"
    task-mgr --dir "$TASK_MGR_DIR" init --from-json "$PRD_FILE" || {
      echo "Error: Failed to initialize database"
      exit 1
    }
  fi
}

# Get steering context for prompt injection
get_steering_context() {
  local context=""

  # Add session guidance if present
  if [ -n "$SESSION_GUIDANCE" ]; then
    context="## Session Guidance (from user)
$SESSION_GUIDANCE
---

"
  fi

  # Add per-iteration steering file if present
  if [ -f "$STEERING_FILE" ] && [ -s "$STEERING_FILE" ]; then
    local steering_content
    steering_content=$(cat "$STEERING_FILE")
    context="${context}## Steering (from steering.md)
$steering_content
---

"
    echo "  [Steering file detected - guidance will be injected]" >&2
  fi

  printf '%s' "$context"
}

# Handle pause signal for interactive steering
handle_pause() {
  if [ ! -f "$PAUSE_FILE" ]; then
    return
  fi

  echo ""
  echo "======================================================"
  echo "  PAUSED - Interactive Steering Mode"
  echo "======================================================"
  echo "Enter guidance for this session (persists across iterations)."
  echo "Press Enter on empty line to continue, or Ctrl+C to abort."
  echo ""
  rm -f "$PAUSE_FILE"

  local pause_input=""
  while IFS= read -r -p "> " line; do
    [ -z "$line" ] && break
    pause_input="${pause_input}${line}"$'\n'
  done

  if [ -n "$pause_input" ]; then
    if [ -n "$SESSION_GUIDANCE" ]; then
      SESSION_GUIDANCE="${SESSION_GUIDANCE}"$'\n'"---"$'\n'"[Guidance added at iteration $1]:"$'\n'"${pause_input}"
    else
      SESSION_GUIDANCE="[Guidance added at iteration $1]:"$'\n'"${pause_input}"
    fi
    echo "Guidance recorded for this session."
  fi
  echo "Resuming..."
  echo ""
}

# Build iteration context from git history
get_iteration_context() {
  local last_commit
  local last_files_list

  last_commit=$(git log --oneline -1 2>/dev/null || echo "none")
  last_files_list=$(git diff --name-only HEAD~1 2>/dev/null | head -20 || echo "none")

  # Store for task-mgr update
  LAST_FILES=$(echo "$last_files_list" | tr '\n' ',' | sed 's/,$//')

  printf '## Previous Iteration Context
Last commit: %s
Files modified:
%s

Use this to find synergistic tasks (matching touchesFiles).
---

' "$last_commit" "$last_files_list"
}

# ============================================================================
# Main Loop
# ============================================================================

main() {
  # Verify prompt file exists
  if [ ! -f "$PROMPT_FILE" ]; then
    echo "Error: Prompt file not found at $PROMPT_FILE"
    exit 1
  fi

  # Verify git repository
  if ! git rev-parse --git-dir > /dev/null 2>&1; then
    echo "Error: Not a git repository"
    exit 1
  fi

  # Check for uncommitted changes
  local uncommitted
  uncommitted=$(git status --porcelain 2>/dev/null)
  if [ -n "$uncommitted" ]; then
    echo "Warning: Uncommitted changes detected"
    if [ "$AUTO_CONFIRM" != true ]; then
      read -r -p "Continue anyway? (y/N) " -n 1
      echo ""
      if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 1
      fi
    fi
  fi

  # Initialize database
  initialize_database

  # Begin a new run
  echo "Starting new run..."
  local run_output
  run_output=$(task-mgr --dir "$TASK_MGR_DIR" --format json run begin)
  RUN_ID=$(echo "$run_output" | jq -r '.run_id // empty')

  if [ -z "$RUN_ID" ]; then
    echo "Error: Failed to start run"
    exit 1
  fi
  echo "Run started: $RUN_ID"

  # Create temp file for output capture
  OUTPUT_FILE=$(mktemp)

  echo ""
  echo "======================================================"
  echo "  Claude Code Loop (task-mgr powered)"
  echo "======================================================"
  echo "  Database: $TASK_MGR_DIR"
  echo "  PRD: ${PRD_FILE:-'(using database)'}"
  echo "  Prompt: $PROMPT_FILE"
  echo "  Run ID: $RUN_ID"
  echo "  Max iterations: $MAX_ITERATIONS"
  echo "======================================================"
  echo ""

  # Main loop
  for i in $(seq 1 "$MAX_ITERATIONS"); do
    echo ""
    echo "======================================================="
    echo "  Iteration $i of $MAX_ITERATIONS"
    echo "======================================================="

    # Check for stop signal
    if [ -f "$STOP_FILE" ]; then
      echo "Stop signal detected (.stop file exists)"
      rm -f "$STOP_FILE"
      GRACEFUL_STOP=true
      break
    fi

    # Handle pause signal
    handle_pause "$i"

    # Get next task with claim
    local next_output
    local after_files_arg=""
    if [ -n "$LAST_FILES" ] && [ "$LAST_FILES" != "none" ]; then
      after_files_arg="--after-files $LAST_FILES"
    fi

    # shellcheck disable=SC2086
    next_output=$(task-mgr --dir "$TASK_MGR_DIR" --format json next --claim --run-id "$RUN_ID" $after_files_arg 2>/dev/null) || {
      # Check if no tasks remaining
      if echo "$next_output" | jq -e '.error' >/dev/null 2>&1; then
        local error_msg
        error_msg=$(echo "$next_output" | jq -r '.error // empty')
        if [[ "$error_msg" == *"No eligible tasks"* ]]; then
          echo ""
          echo "======================================================"
          echo "  All tasks complete!"
          echo "======================================================"
          GRACEFUL_STOP=true
          exit 0
        fi
        echo "Error getting next task: $error_msg"
        exit 1
      fi
      echo "Error getting next task"
      exit 1
    }

    # Extract task information
    CURRENT_TASK_ID=$(echo "$next_output" | jq -r '.task.id // empty')
    local task_title
    task_title=$(echo "$next_output" | jq -r '.task.title // empty')
    local task_json
    task_json=$(echo "$next_output" | jq -c '.task // {}')
    local learnings_json
    learnings_json=$(echo "$next_output" | jq -c '.learnings // []')

    if [ -z "$CURRENT_TASK_ID" ]; then
      echo ""
      echo "======================================================"
      echo "  No more tasks to work on!"
      echo "======================================================"
      GRACEFUL_STOP=true
      break
    fi

    echo "  Task: $CURRENT_TASK_ID - $task_title"

    # Get progress stats
    local stats_output
    stats_output=$(task-mgr --dir "$TASK_MGR_DIR" --format json list 2>/dev/null || echo '[]')
    local total
    total=$(echo "$stats_output" | jq 'length')
    local done_count
    done_count=$(echo "$stats_output" | jq '[.[] | select(.status == "done")] | length')
    echo "  Progress: $done_count/$total tasks complete"
    echo "======================================================="

    # Build prompt
    local steering_context
    steering_context=$(get_steering_context)
    local iteration_context
    iteration_context=$(get_iteration_context)
    local prompt_content
    prompt_content=$(cat "$PROMPT_FILE")

    # Inject task and learnings into prompt
    # SECURITY: The JSON variables below contain user-provided content from PRD files.
    # This is safe because:
    # - jq -c produces properly escaped JSON strings
    # - The full_prompt is passed as a single argument to claude (see below)
    # - DO NOT use eval, backticks, or $() on these values
    local full_prompt
    full_prompt="${steering_context}${iteration_context}
## Current Task
\`\`\`json
${task_json}
\`\`\`

## Relevant Learnings
\`\`\`json
${learnings_json}
\`\`\`

---

${prompt_content}"

    # Run Claude Code
    # SECURITY: $full_prompt is passed as a quoted string argument to claude -p.
    # The JSON content is treated as text by Claude, never executed as shell commands.
    echo ""
    echo "Running Claude Code..."
    claude --print --dangerously-skip-permissions -p "$full_prompt" 2>&1 | tee "$OUTPUT_FILE" || true

    # Check for completion signals
    if grep -q "<promise>COMPLETE</promise>" "$OUTPUT_FILE"; then
      echo ""
      echo "======================================================"
      echo "  Claude Code completed all tasks!"
      echo "======================================================"

      # Mark current task complete
      task-mgr --dir "$TASK_MGR_DIR" complete "$CURRENT_TASK_ID" --run-id "$RUN_ID" 2>/dev/null || true
      GRACEFUL_STOP=true
      exit 0
    fi

    # Check for blocked signal
    if grep -q "<promise>BLOCKED</promise>" "$OUTPUT_FILE"; then
      echo ""
      echo "======================================================"
      echo "  Claude Code is blocked"
      echo "======================================================"

      # Mark task as blocked
      task-mgr --dir "$TASK_MGR_DIR" fail "$CURRENT_TASK_ID" --run-id "$RUN_ID" --error "Blocked by Claude" --status blocked 2>/dev/null || true

      # Continue to next task instead of exiting
      echo "Continuing to next task..."
      continue
    fi

    # Check git for completion evidence (commit with task ID)
    local last_commit_msg
    last_commit_msg=$(git log --oneline -1 2>/dev/null || echo "")
    if echo "$last_commit_msg" | grep -q "\[$CURRENT_TASK_ID\]"; then
      echo ""
      echo "Commit found for task $CURRENT_TASK_ID"

      local commit_hash
      commit_hash=$(git rev-parse HEAD 2>/dev/null || echo "")
      task-mgr --dir "$TASK_MGR_DIR" complete "$CURRENT_TASK_ID" --run-id "$RUN_ID" --commit "$commit_hash" 2>/dev/null || true
    fi

    # Update run metadata
    local last_commit_hash
    last_commit_hash=$(git rev-parse HEAD 2>/dev/null || echo "")
    if [ -n "$last_commit_hash" ] && [ -n "$LAST_FILES" ]; then
      task-mgr --dir "$TASK_MGR_DIR" run update --run-id "$RUN_ID" --last-commit "$last_commit_hash" --last-files "$LAST_FILES" 2>/dev/null || true
    fi

    # Export after every iteration for crash recovery
    if [ -n "$PRD_FILE" ]; then
      task-mgr --dir "$TASK_MGR_DIR" export --to-json "$PRD_FILE" 2>/dev/null || true
    fi

    echo "Iteration $i complete. Continuing..."
    sleep 2
  done

  echo ""
  echo "======================================================"
  if [ "$GRACEFUL_STOP" = true ]; then
    echo "  Gracefully stopped"
  else
    echo "  Reached max iterations ($MAX_ITERATIONS)"
  fi
  echo "======================================================"

  # Final export
  if [ -n "$PRD_FILE" ]; then
    echo "Final export to $PRD_FILE"
    task-mgr --dir "$TASK_MGR_DIR" export --to-json "$PRD_FILE" 2>/dev/null || true
  fi
}

# Run main
main "$@"
