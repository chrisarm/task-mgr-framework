#!/bin/bash
# Rebuild task-mgr database from canonical JSON PRD file
#
# Usage: ./rebuild-from-json.sh [options] <prd_file>
#
# Options:
#   --dir DIR       task-mgr database directory (default: .task-mgr)
#   --force, -f     Skip confirmation prompt
#   -h, --help      Show this help message
#
# WARNING: This will delete the existing database and create a new one.
# All learnings stored in the database will be LOST. Use backup-db.sh
# first if you want to preserve them.
#
# This is a recovery tool for when the database becomes corrupted or
# out of sync with the canonical JSON file.

set -e

# ============================================================================
# Configuration & Defaults
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Default task-mgr database directory
TASK_MGR_DIR="${TASK_MGR_DIR:-.task-mgr}"
FORCE=false
PRD_FILE=""

# ============================================================================
# Argument Parsing
# ============================================================================

show_help() {
  cat << EOF
Usage: $(basename "$0") [options] <prd_file>

Rebuild task-mgr database from canonical JSON PRD file.

Options:
  --dir DIR       task-mgr database directory (default: .task-mgr)
  --force, -f     Skip confirmation prompt
  -h, --help      Show this help message

WARNING: This will delete the existing database and create a new one.
All learnings stored in the database will be LOST. Use backup-db.sh
first if you want to preserve them.

This is a recovery tool for when the database becomes corrupted or
out of sync with the canonical JSON file.

Example:
  $(basename "$0") .task-mgr/tasks/my-project.json
  $(basename "$0") --force .task-mgr/tasks/my-project.json
EOF
}

while [[ $# -gt 0 ]]; do
  case $1 in
    --dir)
      TASK_MGR_DIR="$2"
      shift 2
      ;;
    --force|-f)
      FORCE=true
      shift
      ;;
    -h|--help)
      show_help
      exit 0
      ;;
    -*)
      echo "Error: Unknown option: $1" >&2
      show_help >&2
      exit 1
      ;;
    *)
      PRD_FILE="$1"
      shift
      ;;
  esac
done

# Validate PRD file argument
if [ -z "$PRD_FILE" ]; then
  echo "Error: PRD file path is required" >&2
  show_help >&2
  exit 1
fi

# Resolve paths to absolute
if [[ "$TASK_MGR_DIR" != /* ]]; then
  TASK_MGR_DIR="$(pwd)/$TASK_MGR_DIR"
fi
if [[ "$PRD_FILE" != /* ]]; then
  PRD_FILE="$(pwd)/$PRD_FILE"
fi

# ============================================================================
# Helper Functions
# ============================================================================

error() {
  echo "Error: $*" >&2
  exit 1
}

# ============================================================================
# Dependency Checks
# ============================================================================

check_dependencies() {
  local missing=()

  for cmd in jq task-mgr; do
    if ! command -v "$cmd" &> /dev/null; then
      missing+=("$cmd")
    fi
  done

  if [ ${#missing[@]} -gt 0 ]; then
    error "Required commands not found: ${missing[*]}"
  fi
}

check_dependencies

# ============================================================================
# Main
# ============================================================================

main() {
  local db_file="$TASK_MGR_DIR/tasks.db"

  # Validate PRD file exists
  if [ ! -f "$PRD_FILE" ]; then
    error "PRD file not found: $PRD_FILE"
  fi

  # Validate PRD file is valid JSON
  if ! jq empty "$PRD_FILE" 2>/dev/null; then
    error "PRD file is not valid JSON: $PRD_FILE"
  fi

  # Check if database exists
  local db_exists=false
  if [ -f "$db_file" ]; then
    db_exists=true
  fi

  # Warn about lost learnings if database exists
  if [ "$db_exists" = true ]; then
    echo "=========================================="
    echo "  WARNING: Destructive Operation"
    echo "=========================================="
    echo ""
    echo "This will DELETE the existing database and all learnings!"
    echo ""
    echo "Database: $db_file"
    echo "PRD source: $PRD_FILE"
    echo ""

    # Count learnings that will be lost
    local learnings_count=0
    if command -v task-mgr &> /dev/null; then
      learnings_count=$(task-mgr --dir "$TASK_MGR_DIR" --format json learnings 2>/dev/null | jq 'length' 2>/dev/null || echo "0")
    fi

    if [ "$learnings_count" != "0" ]; then
      echo "LEARNINGS THAT WILL BE LOST: $learnings_count"
      echo ""
      echo "Consider running backup-db.sh first to preserve learnings:"
      echo "  $SCRIPT_DIR/backup-db.sh --dir $TASK_MGR_DIR"
      echo ""
    fi

    # Confirm unless --force
    if [ "$FORCE" != true ]; then
      read -r -p "Are you sure you want to continue? (yes/N) " response
      if [ "$response" != "yes" ]; then
        echo "Aborted."
        exit 1
      fi
    fi
  fi

  echo ""
  echo "Rebuilding database from JSON..."

  # Remove existing database files
  if [ "$db_exists" = true ]; then
    echo "  Removing existing database..."
    rm -f "$db_file" "${db_file}-wal" "${db_file}-shm" "${db_file}.lock"
  fi

  # Create directory if needed
  if [ ! -d "$TASK_MGR_DIR" ]; then
    echo "  Creating directory: $TASK_MGR_DIR"
    mkdir -p "$TASK_MGR_DIR"
  fi

  # Initialize from JSON
  echo "  Initializing from $PRD_FILE..."
  if ! task-mgr --dir "$TASK_MGR_DIR" init --from-json "$PRD_FILE"; then
    error "Failed to initialize database from JSON"
  fi

  # Verify database was created
  if [ ! -f "$db_file" ]; then
    error "Database was not created - check task-mgr output"
  fi

  # Get stats
  local stats
  stats=$(task-mgr --dir "$TASK_MGR_DIR" --format json list 2>/dev/null || echo '[]')
  local task_count
  task_count=$(echo "$stats" | jq 'length' 2>/dev/null || echo "0")

  echo ""
  echo "=========================================="
  echo "  Rebuild Complete"
  echo "=========================================="
  echo ""
  echo "  Database: $db_file"
  echo "  Tasks imported: $task_count"
  echo "  Learnings: 0 (database was reset)"
  echo ""
  echo "Note: Run exports and learnings will need to be re-recorded."
}

main "$@"
