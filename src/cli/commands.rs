//! CLI command definitions for task-mgr.
//!
//! This module contains the Commands enum and all subcommand definitions,
//! including their argument specifications using clap derive macros.

use std::path::PathBuf;

use clap::Subcommand;

use super::enums::{
    Confidence, FailStatus, LearningOutcome, RunEndStatus, Shell, TaskStatusFilter,
};

/// Available commands for task-mgr
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize database from a JSON PRD file
    #[command(after_help = "\
EXAMPLES:
    # Initialize from a single PRD file
    task-mgr init --from-json tasks/my-prd.json

    # Initialize from multiple PRD files
    task-mgr init --from-json phase1.json --from-json phase2.json

    # Re-initialize, dropping existing data
    task-mgr init --from-json prd.json --force

    # Add new tasks to existing database
    task-mgr init --from-json new-tasks.json --append

    # Update existing tasks when IDs match
    task-mgr init --from-json prd.json --append --update-existing

    # Preview changes without modifying database
    task-mgr init --from-json prd.json --dry-run

    # Use explicit prefix for task IDs
    task-mgr init --from-json prd.json --prefix P3

    # Disable auto-prefixing (use raw IDs from JSON)
    task-mgr init --from-json prd.json --no-prefix

TASK ID PREFIXING:
    By default, all task IDs are prefixed to prevent cross-phase collisions.
    The prefix is determined in this order:
      1. --prefix flag (highest priority)
      2. \"taskPrefix\" field in the PRD JSON
      3. Auto-generated 8-char UUID (written back to JSON for stability)
    Use --no-prefix to import task IDs exactly as they appear in the JSON.
")]
    Init {
        /// Path to the JSON PRD file(s) to import
        #[arg(long = "from-json", required = true)]
        from_json: Vec<PathBuf>,

        /// Force re-initialization, dropping existing data
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Append to existing database (preserves existing tasks, learnings, runs)
        #[arg(long, default_value_t = false)]
        append: bool,

        /// Update existing tasks when appending (requires --append)
        #[arg(long = "update-existing", default_value_t = false)]
        update_existing: bool,

        /// Preview what would be changed without making modifications
        #[arg(long = "dry-run", default_value_t = false)]
        dry_run: bool,

        /// Prefix to prepend to all task IDs (e.g., "P3" becomes "P3-FEAT-001").
        /// Overrides the "taskPrefix" field in the PRD JSON.
        /// If neither this flag nor the JSON field is set, a short UUID prefix is
        /// auto-generated and written back to the JSON for stability.
        #[arg(long, conflicts_with = "no_prefix")]
        prefix: Option<String>,

        /// Disable task ID prefixing entirely (import IDs as-is from JSON)
        #[arg(long = "no-prefix", default_value_t = false, conflicts_with = "prefix")]
        no_prefix: bool,
    },

    /// List tasks with optional filtering
    List {
        /// Filter by task status
        #[arg(long, value_enum)]
        status: Option<TaskStatusFilter>,

        /// Filter by file (glob pattern matching against touchesFiles)
        #[arg(long)]
        file: Option<String>,

        /// Filter by task type prefix (e.g., US-, SEC-, FIX-)
        #[arg(long = "task-type")]
        task_type: Option<String>,
    },

    /// Show detailed information about a single task
    Show {
        /// Task ID to display
        task_id: String,
    },

    /// Get the next recommended task to work on
    #[command(after_help = "\
EXAMPLES:
    # Get the next recommended task (read-only)
    task-mgr next

    # Get and claim the next task for this run
    task-mgr next --claim --run-id abc123

    # Use file locality - prefer tasks touching recently modified files
    task-mgr next --claim --after-files src/main.rs,src/lib.rs

    # In a shell script (claude-loop.sh pattern)
    TASK_JSON=$(task-mgr --format json next --claim --run-id \"$RUN_ID\" \\
        --after-files \"$LAST_FILES\")

    # Disable automatic decay of blocked/skipped tasks
    task-mgr next --decay-threshold 0

    # Custom decay threshold (64 iterations)
    task-mgr next --claim --decay-threshold 64
")]
    Next {
        /// Files modified in the previous iteration (for file locality scoring)
        #[arg(long = "after-files", value_delimiter = ',')]
        after_files: Option<Vec<String>>,

        /// Claim the task (set status to in_progress)
        #[arg(long, default_value_t = false)]
        claim: bool,

        /// Associate with a run ID
        #[arg(long = "run-id")]
        run_id: Option<String>,

        /// Decay threshold in iterations for blocked/skipped tasks (0 = disabled)
        /// Tasks blocked/skipped longer than this will auto-reset to todo
        #[arg(long = "decay-threshold", default_value_t = 32)]
        decay_threshold: i64,

        /// Scope task selection to a specific PRD prefix (e.g., "P1" returns only P1-* tasks)
        #[arg(long)]
        prefix: Option<String>,
    },

    /// Mark one or more tasks as completed
    #[command(alias = "done")]
    Complete {
        /// Task ID(s) to mark as complete
        #[arg(required = true)]
        task_ids: Vec<String>,

        /// Associate with a run ID
        #[arg(long = "run-id")]
        run_id: Option<String>,

        /// Commit hash to record
        #[arg(long)]
        commit: Option<String>,

        /// Force completion even if status transition is invalid (e.g., todo->done)
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Mark one or more tasks as failed (blocked, skipped, or irrelevant)
    Fail {
        /// Task ID(s) to mark as failed
        #[arg(required = true)]
        task_ids: Vec<String>,

        /// Associate with a run ID
        #[arg(long = "run-id")]
        run_id: Option<String>,

        /// Error message describing the failure
        #[arg(long)]
        error: Option<String>,

        /// Failure status
        #[arg(long, value_enum, default_value_t = FailStatus::Blocked)]
        status: FailStatus,

        /// Force status change even if transition is invalid (e.g., from done)
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Run lifecycle management (begin, update, end)
    Run {
        #[command(subcommand)]
        action: RunAction,
    },

    /// Export database state to JSON
    Export {
        /// Path to write the JSON PRD file
        #[arg(long = "to-json", required = true)]
        to_json: PathBuf,

        /// Also export progress.json with learnings and runs
        #[arg(long = "with-progress", default_value_t = false)]
        with_progress: bool,

        /// Export learnings to a separate file
        #[arg(long = "learnings-file")]
        learnings_file: Option<PathBuf>,
    },

    /// Check database health and fix stale state
    #[command(after_help = "\
EXAMPLES:
    # Check database health (read-only)
    task-mgr doctor

    # Automatically fix issues found
    task-mgr doctor --auto-fix

    # Preview fixes without applying them
    task-mgr doctor --auto-fix --dry-run

    # Check with custom decay threshold
    task-mgr doctor --decay-threshold 64

    # Disable decay warnings
    task-mgr doctor --decay-threshold 0

    # In shell script for recovery after crash
    task-mgr doctor --auto-fix  # Resets stale in_progress tasks

ISSUES DETECTED:
    - Stale in_progress tasks (no active run, or old started_at)
    - Tasks approaching decay threshold (blocked/skipped for too long)
    - Database integrity issues

COMMON USE CASES:
    # After a crash or interrupted run:
    task-mgr doctor --auto-fix
    # This resets tasks stuck in in_progress to todo status

    # In claude-loop.sh startup:
    task-mgr doctor --auto-fix || task-mgr init --from-json prd.json
")]
    Doctor {
        /// Automatically fix issues found
        #[arg(long = "auto-fix", default_value_t = false)]
        auto_fix: bool,

        /// Preview what would be fixed without making modifications (implies --auto-fix)
        #[arg(long = "dry-run", default_value_t = false)]
        dry_run: bool,

        /// Decay threshold in iterations for checking decay warnings (0 = disabled)
        #[arg(long = "decay-threshold", default_value_t = 32)]
        decay_threshold: i64,

        /// Reconcile git commit history with task status (parse [TASK-ID] from commits)
        #[arg(long = "reconcile-git", default_value_t = false)]
        reconcile_git: bool,
    },

    /// Skip one or more tasks intentionally (defer for later without marking as failed)
    Skip {
        /// Task ID(s) to skip
        #[arg(required = true)]
        task_ids: Vec<String>,

        /// Reason for skipping (required)
        #[arg(long, required = true)]
        reason: String,

        /// Associate with a run ID
        #[arg(long = "run-id")]
        run_id: Option<String>,
    },

    /// Mark one or more tasks as irrelevant (no longer needed due to changed requirements)
    Irrelevant {
        /// Task ID(s) to mark as irrelevant
        #[arg(required = true)]
        task_ids: Vec<String>,

        /// Reason why task is no longer relevant (required)
        #[arg(long, required = true)]
        reason: String,

        /// Associate with a run ID
        #[arg(long = "run-id")]
        run_id: Option<String>,

        /// Learning ID that made this task irrelevant
        #[arg(long = "learning-id")]
        learning_id: Option<i64>,
    },

    /// Record a learning from a task outcome
    #[command(after_help = "\
EXAMPLES:
    # Record a failure learning with root cause and solution
    task-mgr learn --outcome failure \\
        --title 'SQLite busy timeout on concurrent writes' \\
        --content 'When multiple processes write simultaneously, get SQLITE_BUSY' \\
        --root-cause 'Default busy timeout is 0ms' \\
        --solution 'Set PRAGMA busy_timeout = 5000 in connection setup' \\
        --files 'src/db/*.rs' \\
        --tags sqlite,concurrency,database \\
        --confidence high

    # Record a pattern discovery
    task-mgr learn --outcome pattern \\
        --title 'Use COALESCE for optional column updates' \\
        --content 'SQLite UPDATE with COALESCE(?1, column) preserves existing value when NULL passed' \\
        --files 'src/db/*.rs' \\
        --task-types US-,FIX- \\
        --tags sqlite,patterns

    # Record a workaround for a known issue
    task-mgr learn --outcome workaround \\
        --title 'Clap derive requires explicit Clone for ValueEnum' \\
        --content 'Custom enums used with value_enum need #[derive(Clone)]' \\
        --solution 'Add Clone to derive macro list' \\
        --errors 'E0277,Clone is not implemented' \\
        --tags rust,clap,cli

    # Record a success pattern with task association
    task-mgr learn --outcome success \\
        --title 'Transaction wrapper for multi-table updates' \\
        --content 'Wrap related inserts in explicit transaction for atomicity' \\
        --task-id US-015 \\
        --run-id abc123 \\
        --confidence high

OUTCOME TYPES:
    failure    - Learning from an error or bug
    success    - Successful pattern worth remembering
    workaround - Temporary fix for a known limitation
    pattern    - General coding pattern or convention
")]
    Learn {
        /// Type of learning outcome
        #[arg(long, value_enum)]
        outcome: LearningOutcome,

        /// Short title for the learning
        #[arg(long)]
        title: String,

        /// Detailed content of the learning
        #[arg(long)]
        content: String,

        /// Task ID this learning is associated with
        #[arg(long = "task-id")]
        task_id: Option<String>,

        /// Run ID this learning is associated with
        #[arg(long = "run-id")]
        run_id: Option<String>,

        /// Root cause of the issue (for failure/workaround outcomes)
        #[arg(long = "root-cause")]
        root_cause: Option<String>,

        /// Solution that was applied
        #[arg(long)]
        solution: Option<String>,

        /// File patterns this learning applies to (comma-separated)
        #[arg(long, value_delimiter = ',')]
        files: Option<Vec<String>>,

        /// Task type prefixes this learning applies to (comma-separated)
        #[arg(long = "task-types", value_delimiter = ',')]
        task_types: Option<Vec<String>>,

        /// Error patterns this learning applies to (comma-separated)
        #[arg(long, value_delimiter = ',')]
        errors: Option<Vec<String>>,

        /// Tags for categorization (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,

        /// Confidence level for this learning
        #[arg(long, value_enum, default_value_t = Confidence::Medium)]
        confidence: Confidence,
    },

    /// Find relevant learnings for a task or query
    #[command(after_help = "\
EXAMPLES:
    # Find learnings matching a specific task (by file patterns and type)
    task-mgr recall --for-task US-015

    # Search by text query
    task-mgr recall --query 'SQLite connection'

    # Filter by tags
    task-mgr recall --tags sqlite,database

    # Find only failure learnings
    task-mgr recall --outcome failure

    # Combine filters with more results
    task-mgr recall --query 'timeout' --tags database --limit 10

    # Get learnings as JSON for script processing
    task-mgr --format json recall --for-task US-015 | jq '.learnings[].title'

MATCHING BEHAVIOR:
    --for-task matches learnings by:
      - File patterns (glob matching against task's touchesFiles)
      - Task type prefix (e.g., US- tasks match learnings for US-)
      - Error patterns (if task has last_error)

    Results are ranked by last_applied_at (most recently useful first).
")]
    Recall {
        /// Text query to search for in title and content
        #[arg(long)]
        query: Option<String>,

        /// Find learnings matching a specific task's files and type
        #[arg(long = "for-task")]
        for_task: Option<String>,

        /// Filter by tags (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,

        /// Filter by outcome type
        #[arg(long, value_enum)]
        outcome: Option<LearningOutcome>,

        /// Maximum number of results to return
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },

    /// List all learnings
    Learnings {
        /// Show only the N most recent learnings
        #[arg(long)]
        recent: Option<usize>,
    },

    /// Record that a learning was applied (confirmed useful)
    ///
    /// This provides feedback for the UCB bandit ranking system.
    /// Call this when a learning retrieved via `recall` was actually
    /// useful for completing the current task.
    #[command(name = "apply-learning")]
    ApplyLearning {
        /// ID of the learning that was applied
        learning_id: i64,
    },

    /// Return a blocked task to todo status for retry
    Unblock {
        /// Task ID to unblock
        task_id: String,
    },

    /// Return a skipped task to todo status for retry
    Unskip {
        /// Task ID to unskip
        task_id: String,
    },

    /// Reset task(s) to todo status for re-running
    Reset {
        /// Task ID(s) to reset (omit for --all)
        #[arg(required_unless_present = "all")]
        task_ids: Vec<String>,

        /// Reset all non-todo tasks
        #[arg(long, default_value_t = false)]
        all: bool,

        /// Skip confirmation prompt for --all
        #[arg(long, short = 'y', default_value_t = false)]
        yes: bool,
    },

    /// Show progress summary (task counts, completion rate, learnings)
    Stats,

    /// Show run history
    History {
        /// Maximum number of runs to show
        #[arg(long, default_value_t = 10)]
        limit: usize,

        /// Show detailed information for a specific run
        #[arg(long = "run-id")]
        run_id: Option<String>,
    },

    /// Delete a learning from the database
    DeleteLearning {
        /// Learning ID to delete
        learning_id: i64,

        /// Skip confirmation prompt
        #[arg(long, short = 'y', default_value_t = false)]
        yes: bool,
    },

    /// Edit an existing learning
    EditLearning {
        /// Learning ID to edit
        learning_id: i64,

        /// New title for the learning
        #[arg(long)]
        title: Option<String>,

        /// New content for the learning
        #[arg(long)]
        content: Option<String>,

        /// New solution for the learning
        #[arg(long)]
        solution: Option<String>,

        /// New root cause for the learning
        #[arg(long = "root-cause")]
        root_cause: Option<String>,

        /// New confidence level
        #[arg(long, value_enum)]
        confidence: Option<Confidence>,

        /// Tags to add (comma-separated)
        #[arg(long = "add-tags", value_delimiter = ',')]
        add_tags: Option<Vec<String>>,

        /// Tags to remove (comma-separated)
        #[arg(long = "remove-tags", value_delimiter = ',')]
        remove_tags: Option<Vec<String>>,

        /// File patterns to add (comma-separated)
        #[arg(long = "add-files", value_delimiter = ',')]
        add_files: Option<Vec<String>>,

        /// File patterns to remove (comma-separated)
        #[arg(long = "remove-files", value_delimiter = ',')]
        remove_files: Option<Vec<String>>,
    },

    /// Review blocked and skipped tasks
    Review {
        /// Only review blocked tasks
        #[arg(long, default_value_t = false)]
        blocked: bool,

        /// Only review skipped tasks
        #[arg(long, default_value_t = false)]
        skipped: bool,

        /// Automatically unblock/unskip all tasks without prompts (for scripting)
        #[arg(long, default_value_t = false)]
        auto: bool,
    },

    /// Manage database schema migrations
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },

    /// Generate shell completions
    #[command(after_help = "\
EXAMPLES:
    # Generate bash completions
    task-mgr completions bash > /etc/bash_completion.d/task-mgr

    # Generate zsh completions (add to fpath)
    task-mgr completions zsh > ~/.zsh/completions/_task-mgr

    # Generate fish completions
    task-mgr completions fish > ~/.config/fish/completions/task-mgr.fish

    # Generate PowerShell completions
    task-mgr completions powershell >> $PROFILE

INSTALLATION:
    Bash:
        sudo task-mgr completions bash > /etc/bash_completion.d/task-mgr
        # Or for user-only: task-mgr completions bash > ~/.local/share/bash-completion/completions/task-mgr

    Zsh:
        task-mgr completions zsh > ~/.zsh/completions/_task-mgr
        # Ensure ~/.zsh/completions is in your fpath

    Fish:
        task-mgr completions fish > ~/.config/fish/completions/task-mgr.fish

    PowerShell:
        task-mgr completions powershell >> $PROFILE
        # Restart PowerShell to load completions
")]
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },

    /// Run autonomous agent loop
    #[command(after_help = "\
EXAMPLES:
    # Run loop with a PRD file (will prompt for confirmation)
    task-mgr loop tasks/my-prd.json

    # Run loop with auto-confirmation
    task-mgr loop tasks/my-prd.json --yes

    # Run loop with time budget
    task-mgr loop tasks/my-prd.json --yes --hours 4.5

    # Run loop with custom prompt file
    task-mgr loop tasks/my-prd.json --prompt-file tasks/custom-prompt.md --yes

    # Run loop with verbose output
    task-mgr loop tasks/my-prd.json --yes --verbose

    # Run without git worktrees (use branch checkout instead)
    task-mgr loop tasks/my-prd.json --no-worktree
")]
    Loop {
        /// Path to PRD JSON file
        prd_file: PathBuf,

        /// Path to prompt file (default: <base>-prompt.md)
        #[arg(long)]
        prompt_file: Option<PathBuf>,

        /// Auto-confirm all prompts
        #[arg(short = 'y', long)]
        yes: bool,

        /// Time budget in hours (max 168)
        #[arg(long)]
        hours: Option<f64>,

        /// Verbose output
        #[arg(long)]
        verbose: bool,

        /// Disable git worktrees (use branch checkout instead)
        ///
        /// By default, task-mgr uses git worktrees to avoid branch switching
        /// conflicts when there are uncommitted changes. This flag reverts
        /// to the old behavior of checking out branches directly.
        #[arg(long, default_value_t = false)]
        no_worktree: bool,

        /// Path to external git repo for commit scanning (overrides PRD value)
        ///
        /// When Claude commits to a different repo than the working directory
        /// (e.g., an Elixir project), specify the path here so task-mgr can
        /// scan its commits for task completion evidence.
        #[arg(long = "external-repo")]
        external_repo: Option<PathBuf>,

        /// Remove the git worktree on loop exit (requires worktree mode)
        ///
        /// When set, the worktree created for this loop run will be removed
        /// after the loop finishes. Dirty worktrees are warned but not forced.
        /// In interactive mode (no --yes), the user is prompted instead.
        #[arg(long = "cleanup-worktree", default_value_t = false)]
        cleanup_worktree: bool,
    },

    /// Show status dashboard for PRD projects
    Status {
        /// Path to PRD JSON file (optional, shows all projects if omitted)
        prd_file: Option<PathBuf>,

        /// Show detailed task listing
        #[arg(short = 'v', long)]
        verbose: bool,

        /// Filter to a single PRD by task ID prefix (e.g., "9c5c8a1d")
        #[arg(long)]
        prefix: Option<String>,
    },

    /// Run multiple PRDs in sequence
    #[command(after_help = "\
EXAMPLES:
    # Run all PRD files matching a pattern
    task-mgr batch 'tasks/*.json' --yes

    # Run with max iterations per PRD
    task-mgr batch 'tasks/*.json' 10 --yes

    # Keep worktrees after each PRD (default: auto-remove on success)
    task-mgr batch 'tasks/*.json' --yes --keep-worktrees
")]
    Batch {
        /// Glob pattern to match PRD files
        pattern: String,

        /// Maximum iterations per PRD
        max_iterations: Option<usize>,

        /// Auto-confirm all prompts
        #[arg(short = 'y', long)]
        yes: bool,

        /// Keep worktrees after each PRD completes (never auto-remove)
        ///
        /// By default, worktrees are removed on success in --yes mode,
        /// and the user is prompted in interactive mode.
        /// This flag skips cleanup entirely.
        #[arg(long = "keep-worktrees", default_value_t = false)]
        keep_worktrees: bool,
    },

    /// Import learnings from a progress.json or learnings JSON file
    #[command(
        name = "import-learnings",
        after_help = "\
EXAMPLES:
    # Import learnings from a progress.json file
    task-mgr import-learnings --from-json progress.json

    # Import and reset bandit statistics
    task-mgr import-learnings --from-json learnings.json --reset-stats
"
    )]
    ImportLearnings {
        /// Path to the JSON file to import (progress.json or learnings array)
        #[arg(long = "from-json", required = true)]
        from_json: PathBuf,

        /// Reset bandit statistics (times_shown, times_applied) on import
        #[arg(long = "reset-stats", default_value_t = false)]
        reset_stats: bool,
    },

    /// Archive completed PRDs and extract learnings
    Archive {
        /// Preview what would be archived without moving files
        #[arg(long)]
        dry_run: bool,
    },

    /// Extract learnings from a Claude output file using LLM analysis
    #[command(
        name = "extract-learnings",
        after_help = "\
EXAMPLES:
    # Extract learnings from saved Claude output
    task-mgr extract-learnings --from-output /tmp/claude-output.txt

    # Extract with task context
    task-mgr extract-learnings --from-output output.txt --task-id US-001

    # Disable auto-extraction in the loop (env var)
    TASK_MGR_NO_EXTRACT_LEARNINGS=1 task-mgr loop tasks/prd.json --yes
"
    )]
    ExtractLearnings {
        /// Path to file containing Claude's output
        #[arg(long = "from-output", required = true)]
        from_output: PathBuf,

        /// Task ID to associate with extracted learnings
        #[arg(long = "task-id")]
        task_id: Option<String>,

        /// Run ID to associate with extracted learnings
        #[arg(long = "run-id")]
        run_id: Option<String>,
    },

    /// Manage git worktrees (list, prune, remove)
    Worktrees {
        #[command(subcommand)]
        action: WorktreesAction,
    },

    /// Generate man pages for task-mgr and all subcommands
    #[command(after_help = "\
EXAMPLES:
    # Generate all man pages to a directory
    task-mgr man-pages --output-dir /usr/local/share/man/man1

    # Generate to user-local directory
    mkdir -p ~/.local/share/man/man1
    task-mgr man-pages --output-dir ~/.local/share/man/man1

    # List what would be generated (dry run)
    task-mgr man-pages --list

    # Generate a single man page to stdout (for piping)
    task-mgr man-pages --name task-mgr

INSTALLATION:
    System-wide (requires root):
        sudo task-mgr man-pages --output-dir /usr/local/share/man/man1
        sudo mandb  # Update man database

    User-only:
        mkdir -p ~/.local/share/man/man1
        task-mgr man-pages --output-dir ~/.local/share/man/man1
        mandb -u  # Update user man database

    After installation, view with:
        man task-mgr
        man task-mgr-next
        man task-mgr-learn

GENERATED MAN PAGES:
    - task-mgr.1          Main command
    - task-mgr-init.1     Initialize database
    - task-mgr-next.1     Get next task
    - task-mgr-complete.1 Complete tasks
    - ... and all other subcommands
")]
    ManPages {
        /// Output directory for man pages (generates all .1 files)
        #[arg(long = "output-dir")]
        output_dir: Option<std::path::PathBuf>,

        /// Generate a specific man page to stdout (e.g., 'task-mgr', 'task-mgr-next')
        #[arg(long)]
        name: Option<String>,

        /// List all available man page names without generating
        #[arg(long, default_value_t = false)]
        list: bool,
    },
}

/// Worktrees subcommand actions
#[derive(Subcommand, Debug)]
pub enum WorktreesAction {
    /// List all git worktrees with branch, path, and lock status
    List,

    /// Remove unlocked worktrees (skips locked and dirty)
    Prune,

    /// Remove a specific worktree by path or branch name
    Remove {
        /// Path or branch name of the worktree to remove
        target: String,
    },
}

/// Migration actions for manual schema control
#[derive(Subcommand, Debug)]
pub enum MigrateAction {
    /// Show current migration status
    Status,

    /// Apply the next pending migration
    Up,

    /// Revert the most recent migration
    Down,

    /// Apply all pending migrations (default behavior on db open)
    All,
}

/// Run lifecycle actions
#[derive(Subcommand, Debug)]
pub enum RunAction {
    /// Begin a new run session
    Begin,

    /// Update an active run with progress information
    Update {
        /// Run ID to update
        #[arg(long = "run-id", required = true)]
        run_id: String,

        /// Last commit hash made during this run
        #[arg(long = "last-commit")]
        last_commit: Option<String>,

        /// Files modified in the last iteration (comma-separated)
        #[arg(long = "last-files", value_delimiter = ',')]
        last_files: Option<Vec<String>>,
    },

    /// End a run session
    End {
        /// Run ID to end
        #[arg(long = "run-id", required = true)]
        run_id: String,

        /// Final status of the run
        #[arg(long, value_enum)]
        status: RunEndStatus,
    },
}
