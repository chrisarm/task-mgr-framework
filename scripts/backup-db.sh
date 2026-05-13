#!/bin/bash
# Backup task-mgr database with timestamp naming
#
# Usage: ./backup-db.sh [options]
#
# Options:
#   --dir DIR       task-mgr database directory (default: .task-mgr)
#   -q, --quiet     Suppress output except errors
#   -h, --help      Show this help message
#
# Creates backups in .task-mgr/backups/ directory with names like:
#   tasks.db.2024-01-15-143022
#
# The backup includes the main database and WAL files if present.

set -e

# ============================================================================
# Configuration & Defaults
# ============================================================================

# Default task-mgr database directory
TASK_MGR_DIR="${TASK_MGR_DIR:-.task-mgr}"
QUIET=false

# ============================================================================
# Argument Parsing
# ============================================================================

show_help() {
  cat << EOF
Usage: $(basename "$0") [options]

Backup task-mgr database with timestamp naming.

Options:
  --dir DIR       task-mgr database directory (default: .task-mgr)
  -q, --quiet     Suppress output except errors
  -h, --help      Show this help message

Creates backups in .task-mgr/backups/ directory with names like:
  tasks.db.2024-01-15-143022

The backup includes the main database and WAL files if present.
EOF
}

while [[ $# -gt 0 ]]; do
  case $1 in
    --dir)
      TASK_MGR_DIR="$2"
      shift 2
      ;;
    -q|--quiet)
      QUIET=true
      shift
      ;;
    -h|--help)
      show_help
      exit 0
      ;;
    *)
      echo "Error: Unknown option: $1" >&2
      show_help >&2
      exit 1
      ;;
  esac
done

# Resolve paths to absolute
if [[ "$TASK_MGR_DIR" != /* ]]; then
  TASK_MGR_DIR="$(pwd)/$TASK_MGR_DIR"
fi

# ============================================================================
# Helper Functions
# ============================================================================

log() {
  if [ "$QUIET" != true ]; then
    echo "$@"
  fi
}

error() {
  echo "Error: $*" >&2
  exit 1
}

# ============================================================================
# Main
# ============================================================================

main() {
  local db_file="$TASK_MGR_DIR/tasks.db"
  local backups_dir="$TASK_MGR_DIR/backups"
  local timestamp
  timestamp=$(date +%Y-%m-%d-%H%M%S)
  local backup_name="tasks.db.$timestamp"

  # Validate database exists
  if [ ! -f "$db_file" ]; then
    error "Database not found at $db_file"
  fi

  # Create backups directory if needed
  if [ ! -d "$backups_dir" ]; then
    log "Creating backups directory: $backups_dir"
    mkdir -p "$backups_dir"
  fi

  # Perform the backup
  log "Backing up database..."
  log "  Source: $db_file"
  log "  Destination: $backups_dir/$backup_name"

  # Copy the main database file
  cp "$db_file" "$backups_dir/$backup_name"

  # Copy WAL file if it exists (important for active databases)
  if [ -f "${db_file}-wal" ]; then
    log "  Including WAL file..."
    cp "${db_file}-wal" "$backups_dir/${backup_name}-wal"
  fi

  # Copy SHM file if it exists (shared memory)
  if [ -f "${db_file}-shm" ]; then
    log "  Including SHM file..."
    cp "${db_file}-shm" "$backups_dir/${backup_name}-shm"
  fi

  # Verify backup
  if [ -f "$backups_dir/$backup_name" ]; then
    local size
    size=$(du -h "$backups_dir/$backup_name" | cut -f1)
    log ""
    log "Backup complete!"
    log "  File: $backups_dir/$backup_name"
    log "  Size: $size"
  else
    error "Backup verification failed - file not created"
  fi
}

main "$@"
