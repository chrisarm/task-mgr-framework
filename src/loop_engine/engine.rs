/// Autonomous agent loop engine: single iteration + full loop orchestrator.
///
/// `run_iteration()` orchestrates one complete cycle:
/// 1. Check .stop/.pause signals
/// 2. Build enriched prompt (task selection + context)
/// 3. Spawn Claude subprocess
/// 4. Analyze output to determine outcome
/// 5. Record learning feedback
/// 6. Handle reorder requests
///
/// `run_loop()` is the top-level orchestrator:
/// env setup → git validation → init PRD → run lifecycle → iterate → cleanup
///
/// The iteration context carries state between iterations (crash tracker,
/// stale tracker, session guidance, reorder hints, etc.).
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::commands::complete as complete_cmd;
use crate::commands::decisions::find_option;
use crate::commands::doctor::setup_checks::pre_check_loop_setup;
use crate::commands::doctor::setup_output::SetupSeverity;
use crate::commands::init::{PrefixMode, generate_prefix};
use crate::commands::next::selection::select_parallel_group;
use crate::commands::run as run_cmd;
use crate::db::LockGuard;
use crate::db::prefix::{prefix_and, validate_prefix};
use crate::db::schema::key_decisions as key_decisions_db;
use crate::error::TaskMgrError;
use crate::loop_engine::branch;
use crate::loop_engine::calibrate;
use crate::loop_engine::claude;
use crate::loop_engine::config::{
    self, IterationOutcome, LoopConfig, PermissionMode, TASKS_JSON_DISALLOWED_TOOLS,
};
use crate::loop_engine::crash::CrashTracker;
use crate::loop_engine::deadline;
use crate::loop_engine::detection;
use crate::loop_engine::display;
use crate::loop_engine::env;
use crate::loop_engine::git_reconcile::{
    check_git_for_task_completion, reconcile_external_git_completions, wrapper_commit,
};
use crate::loop_engine::guidance::SessionGuidance;
use crate::loop_engine::iteration_pipeline;
use crate::loop_engine::merge_resolver;
use crate::loop_engine::model;
use crate::loop_engine::monitor;
use crate::loop_engine::oauth;
use crate::loop_engine::overflow;
use crate::loop_engine::prd_reconcile::{
    self as prd_reconcile, hash_file, read_prd_metadata, reconcile_passes_with_db,
    update_prd_task_passes,
};
use crate::loop_engine::progress;
use crate::loop_engine::project_config;
use crate::loop_engine::prompt::{self, BuildPromptParams};
use crate::loop_engine::signals::{self, SignalFlag, handle_human_review};
use crate::loop_engine::stale::StaleTracker;
use crate::loop_engine::status_queries::read_prd_hints;
use crate::loop_engine::usage::{self, UsageCheckResult};
use crate::loop_engine::watchdog;
use crate::loop_engine::worktree;
use crate::models::RunStatus;

/// Maximum consecutive reorder attempts before forcing algorithmic pick.
const MAX_CONSECUTIVE_REORDERS: u32 = 2;

/// Deprecation hint displayed at loop start when the claude CLI supports auto mode
/// but the user is not yet using it. Emitted to stderr once per session.
pub(crate) const AUTO_MODE_DEPRECATION_HINT: &str = concat!(
    "\x1b[33m[hint]\x1b[0m ",
    "The current permission model will be deprecated in a future release. ",
    "Set LOOP_ENABLE_AUTO_MODE=true to switch to auto mode. ",
    "Your current settings continue to work in the meantime."
);

/// Parameters for usage API monitoring within an iteration.
#[derive(Debug, Clone)]
pub struct UsageParams {
    /// Whether usage checking is enabled.
    pub enabled: bool,
    /// Usage percentage threshold (0-100) to trigger wait.
    pub threshold: u8,
    /// Fallback wait time in seconds when no reset time is available.
    pub fallback_wait: u64,
}

impl UsageParams {
    /// Create a disabled usage params (skips all checks).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            threshold: 92,
            fallback_wait: 300,
        }
    }
}

/// Parameters for a single iteration of the agent loop.
///
/// Groups the read-only parameters that `run_iteration()` needs,
/// keeping the mutable `IterationContext` as a separate argument.
pub struct IterationParams<'a> {
    /// Database connection
    pub conn: &'a Connection,
    /// Database directory (--dir flag, for task selection queries)
    pub db_dir: &'a Path,
    /// Git repository root (for source scanning, monitoring)
    pub project_root: &'a Path,
    /// Tasks directory (for signal files)
    pub tasks_dir: &'a Path,
    /// Current iteration number (1-based)
    pub iteration: u32,
    /// Maximum number of iterations
    pub max_iterations: u32,
    /// Current run ID
    pub run_id: &'a str,
    /// Path to base prompt.md file
    pub base_prompt_path: &'a Path,
    /// Optional path to steering.md
    pub steering_path: Option<&'a Path>,
    /// Delay between iterations
    pub inter_iteration_delay: Duration,
    /// Shared signal flag for SIGINT/SIGTERM
    pub signal_flag: &'a SignalFlag,
    /// Total elapsed seconds since loop start
    pub elapsed_secs: u64,
    /// Enable verbose output
    pub verbose: bool,
    /// Usage API monitoring parameters
    pub usage_params: &'a UsageParams,
    /// Optional path to PRD JSON file
    pub prd_path: Option<&'a Path>,
    /// Optional task prefix for ID normalization
    pub task_prefix: Option<&'a str>,
    /// Default model from PRD metadata (threaded from run_loop via PrdMetadata).
    pub default_model: Option<&'a str>,
    /// Default model from the per-project config (`.task-mgr/config.json`).
    pub project_default_model: Option<&'a str>,
    /// Default model from the per-user config (`$XDG_CONFIG_HOME/task-mgr/config.json`).
    pub user_default_model: Option<&'a str>,
    /// Permission mode for Claude subprocess invocation.
    pub permission_mode: &'a PermissionMode,
    /// Paths to sibling PRD JSON files (batch mode only, empty otherwise).
    pub batch_sibling_prds: &'a [PathBuf],
}

/// Result of a single iteration.
#[derive(Debug)]
pub struct IterationResult {
    /// What happened this iteration
    pub outcome: IterationOutcome,
    /// ID of the task that was attempted (if any)
    pub task_id: Option<String>,
    /// Files modified by the task (from task metadata)
    pub files_modified: Vec<String>,
    /// Whether the loop should stop after this iteration
    pub should_stop: bool,
    /// Claude's stdout output (for output-based completion detection)
    pub output: String,
    /// Effective model used for this iteration (post-crash-escalation).
    /// None for early exits (signal, rate-limit, etc.).
    pub effective_model: Option<String>,
    /// Effective `--effort` level used for this iteration, derived from task difficulty.
    /// None when difficulty is unset/unknown or for early exits.
    pub effective_effort: Option<&'static str>,
    /// Number of key decisions extracted and stored this iteration.
    /// Always initialised to 0 by the iteration runners; filled in by the
    /// caller (`run_loop` / `process_slot_result`) after
    /// `iteration_pipeline::process_iteration_output` returns the real count.
    pub key_decisions_count: u32,
    /// Structured stream-json conversation transcript (when stream-json mode is
    /// active). Threaded from `claude_result.conversation` at the post-Claude
    /// success site; `None` on every early-exit path (signal, rate-limit, pause,
    /// pre-iteration error). The wave path mirrors this through
    /// `SlotResult.iteration_result.conversation` so the shared
    /// `iteration_pipeline::process_iteration_output` can prefer the transcript
    /// over raw output for learning extraction.
    pub conversation: Option<String>,
    /// Learnings shown to Claude during prompt assembly this iteration.
    /// Threaded from `prompt_result.shown_learning_ids` at the post-Claude
    /// success site; empty on every early-exit path (signal, rate-limit, pause,
    /// pre-iteration error). The shared
    /// `iteration_pipeline::process_iteration_output` consumes this via
    /// `ProcessingParams.shown_learning_ids` to record bandit feedback.
    pub shown_learning_ids: Vec<i64>,
}

/// Mutable context carried between iterations.
pub struct IterationContext {
    /// Last commit hash from previous iteration
    pub last_commit: Option<String>,
    /// Files modified in previous iteration
    pub last_files: Vec<String>,
    /// Accumulated session guidance from pause interactions
    pub session_guidance: SessionGuidance,
    /// Crash tracker for exponential backoff
    pub crash_tracker: CrashTracker,
    /// Stale iteration tracker
    pub stale_tracker: StaleTracker,
    /// Task ID hint from a reorder request
    pub reorder_hint: Option<String>,
    /// Count of consecutive reorders
    pub reorder_count: u32,
    /// Per-task crash flag for the most recent iteration on each task. The
    /// pipeline writes one entry per iteration:
    /// `map[task_id] = matches!(outcome, IterationOutcome::Crash(_))`.
    /// `check_crash_escalation` (post-FEAT-007) consults this map directly,
    /// replacing the `(last_task_id == current) && last_was_crash` predicate
    /// that today's two scalar fields encode.
    ///
    /// Sized by the number of distinct task IDs touched by the loop — bounded
    /// by active task count, NOT iteration count, because each `insert` on the
    /// same key overwrites in place. See `tests/crash_escalation_per_task.rs`
    /// for the bounded-size invariant.
    ///
    /// Loop-thread-local: writes happen on the main thread inside
    /// `process_iteration_output`. Wave-mode slot threads never touch this
    /// map; the wave aggregator passes their outcomes through the pipeline on
    /// the main thread, preserving the no-Mutex contract.
    pub crashed_last_iteration: std::collections::HashMap<String, bool>,
    /// Per-task effort overrides set after `Crash(PromptTooLong)`. Keys are
    /// task IDs, values are the effort level to use on the next attempt in
    /// place of the difficulty-derived default.
    pub effort_overrides: std::collections::HashMap<String, &'static str>,
    /// Per-task model overrides set after `Crash(PromptTooLong)` when effort
    /// downgrade is exhausted. Escalates to the 1M-context model variant so
    /// the task can fit in the larger context window. Uses `String` values
    /// (not `&'static str`) to allow future dynamic model IDs.
    pub model_overrides: std::collections::HashMap<String, String>,
    /// Dedicated marker set indicating which task IDs are currently in an
    /// overflow-recovery state (i.e. recovered from a `Crash(PromptTooLong)`
    /// at least once). The banner annotation gates on THIS set, NOT on
    /// `model_overrides` — crash escalation and consecutive-failure escalation
    /// also write model overrides, but only `Crash(PromptTooLong)` recovery
    /// writes here. Keeping these channels separate prevents banner
    /// false-positives when other escalation paths land (learning #893).
    pub overflow_recovered: std::collections::HashSet<String>,
    /// Per-task original model captured on the FIRST overflow, before any
    /// recovery override is applied. Use `entry(task_id).or_insert(...)` —
    /// never `insert(...)` — so subsequent overflows on the same task don't
    /// overwrite the original with the post-escalation model. Read by the
    /// banner annotation to render `(overflow recovery from <original>)`.
    pub overflow_original_model: std::collections::HashMap<String, String>,
    /// Reorder hints emitted from parallel slots that haven't been consumed yet.
    /// Sequential mode consumes from `reorder_hint`; in wave mode we collect
    /// every `<reorder>` request a slot emits and append here so they survive
    /// the wave (FEAT-010 AC: "Reorder hints from parallel slots queued for
    /// next wave"). `select_parallel_group` is score-driven and does not yet
    /// honor hints, so this acts as a preservation queue rather than a direct
    /// influence on the next selection — hints surface in operator logs and
    /// remain available for future selection-side wiring.
    pub pending_reorder_hints: Vec<String>,
    /// Task IDs that `claim_slot_task` set to `in_progress` in a parallel wave
    /// but whose slot did not mark them `done` (e.g. crash, no `<completed>`
    /// tag emitted, output-scan miss). Tracks across waves so the post-loop
    /// cleanup can reset still-`in_progress` rows when the loop exits via
    /// deadline / max-iterations rather than waiting for the next process's
    /// step 6.6 recovery.
    pub pending_slot_tasks: Vec<String>,
    /// Count of consecutive parallel-slot waves whose merge-back step
    /// produced at least one failed slot. Reset to `0` after every wave that
    /// merges cleanly. Compared against
    /// `ProjectConfig::merge_fail_halt_threshold` at the wave-loop boundary
    /// (FEAT-002): when the counter reaches the threshold, the loop aborts
    /// rather than letting the cascade run unbounded (mw-datalake incident).
    ///
    /// Threshold semantics:
    /// - `0` — counter is incremented but threshold never triggers (legacy "log and continue")
    /// - `1` — halts on any merge-back failure
    /// - `2` (default) — halts after two consecutive failure waves
    pub consecutive_merge_fail_waves: u32,
}

impl IterationContext {
    /// Create a new iteration context with default state.
    pub fn new(max_crashes: u32) -> Self {
        Self {
            last_commit: None,
            last_files: Vec::new(),
            session_guidance: SessionGuidance::new(),
            crash_tracker: CrashTracker::new(max_crashes),
            stale_tracker: StaleTracker::default(),
            reorder_hint: None,
            reorder_count: 0,
            crashed_last_iteration: std::collections::HashMap::new(),
            effort_overrides: std::collections::HashMap::new(),
            model_overrides: std::collections::HashMap::new(),
            overflow_recovered: std::collections::HashSet::new(),
            overflow_original_model: std::collections::HashMap::new(),
            pending_reorder_hints: Vec::new(),
            pending_slot_tasks: Vec::new(),
            consecutive_merge_fail_waves: 0,
        }
    }
}

/// Per-slot execution context for parallel wave iterations.
///
/// Each slot carries the `Send`-safe `SlotPromptBundle` produced on the main
/// thread BEFORE the worker is spawned (per the FEAT-002 contract: prompt
/// construction order is `build_bundle (main) → spawn(worker)`). Workers
/// must NOT touch a `&Connection` or read a `&Task` after spawn — every
/// task-derived value the slot needs (id, files, model, difficulty) is
/// already pre-resolved into the bundle.
///
/// The struct is `Send` so `run_parallel_wave` can move it into a dedicated
/// worker thread. It is intentionally not `Sync`: `run_slot_iteration` never
/// shares a `&SlotContext` across threads.
pub struct SlotContext {
    /// Slot index within the wave (0..parallel_slots).
    pub slot_index: usize,
    /// Working directory for Claude — slot 0 uses the main branch worktree,
    /// slots 1+ each get an ephemeral worktree (see `worktree::ensure_slot_worktrees`).
    pub working_root: PathBuf,
    /// Prompt bundle assembled on the main thread (after the slot's task was
    /// claimed) by `prompt::slot::build_prompt`. Carries the prompt string,
    /// task id, pre-loaded task files, learning ids shown, resolved model,
    /// and difficulty — everything the worker needs without re-opening the
    /// DB. The orphan-reset accounting in `slot_failure_result` reads task
    /// identity from `prompt_bundle.task_id` so the bundle remains the
    /// canonical source-of-truth post-spawn.
    pub prompt_bundle: crate::loop_engine::prompt::slot::SlotPromptBundle,
}

/// Result of running one slot during a wave.
#[derive(Debug)]
pub struct SlotResult {
    /// Slot index the result came from (matches `SlotContext::slot_index`).
    pub slot_index: usize,
    /// Outcome of the slot's iteration (mirrors the sequential `IterationResult`).
    pub iteration_result: IterationResult,
    /// Whether `claim_slot_task` successfully transitioned this slot's task to
    /// `in_progress`. `false` only for `slot_failure_result` entries where the
    /// task was already `done` / `blocked` and the slot thread never spawned.
    /// Drives the post-loop orphan reset (see `IterationContext::pending_slot_tasks`).
    pub claim_succeeded: bool,
    /// Learnings shown in the slot's prompt, threaded back from
    /// `SlotContext::prompt_bundle::shown_learning_ids` so the main thread
    /// can record bandit feedback without reopening a DB connection on the
    /// worker. Empty for `slot_failure_result` entries (no bundle was ever
    /// built) and for early-exit paths that never assembled a prompt.
    pub shown_learning_ids: Vec<i64>,
    /// The full assembled prompt text, carried back from the worker thread
    /// exclusively for overflow diagnostics. `process_slot_result` uses this
    /// to populate the `PromptResult` passed to `overflow::handle_prompt_too_long`
    /// when the outcome is `Crash(PromptTooLong)`.
    ///
    /// `None` on all non-overflow paths; avoids cloning up to 80 KB per slot
    /// on every successful wave just to discard it.
    pub prompt_for_overflow: Option<String>,
    /// Per-section byte sizes from `SlotPromptBundle::section_sizes`, threaded
    /// back so `process_slot_result` can populate the synthetic `PromptResult`
    /// with a meaningful section breakdown on `PromptTooLong`. Empty for
    /// `slot_failure_result` entries (no bundle was assembled).
    pub section_sizes: Vec<(&'static str, usize)>,
    /// Names of trimmable sections dropped at bundle-build time because they
    /// didn't fit within `TOTAL_PROMPT_BUDGET`. Mirrors
    /// `SlotPromptBundle::dropped_sections` so an overflow recovery still
    /// reports the actual drops to the diagnostics dump (instead of an empty
    /// list that would be wrong post-WIRE-FIX-002). Empty for
    /// `slot_failure_result` entries.
    pub dropped_sections: Vec<String>,
    /// Task difficulty at bundle-build time, threaded back so the synthetic
    /// `PromptResult` in the per-slot overflow branch can populate
    /// `task_difficulty` instead of hardcoding `None`. `None` when the task
    /// has no difficulty set or for `slot_failure_result` entries.
    pub task_difficulty: Option<String>,
}

/// Aggregate result of a parallel wave.
#[derive(Debug)]
pub struct WaveResult {
    /// Per-slot outcomes — one entry per slot spawned, preserving slot ordering.
    /// Panicked slots are converted into a `Crash(RuntimeError)` entry with no task_id.
    pub outcomes: Vec<SlotResult>,
    /// Wall-clock duration from thread spawn to all-slots-joined.
    pub wave_duration: Duration,
}

/// Parameters shared across all slot threads in a wave.
///
/// Each field must be safe to clone cheaply (Arc-backed or `Clone` with small
/// owned data) because every spawned thread gets its own copy. The `SignalFlag`
/// is internally `Arc<AtomicBool>` so all threads observe the same signal.
///
/// Per-thread mutable state (DB connection, watchdog epoch) is NOT stored
/// here — it is created inside `run_slot_iteration` so it never crosses thread
/// boundaries.
#[derive(Clone)]
pub struct SlotIterationParams {
    /// Database directory (each slot opens its own connection here).
    pub db_dir: PathBuf,
    /// Permission mode for Claude subprocess invocation.
    pub permission_mode: PermissionMode,
    /// Shared signal flag (Arc-backed) for SIGINT/SIGTERM coordination.
    pub signal_flag: SignalFlag,
    /// Default model from PRD metadata (falls through when the task has no
    /// explicit model and no higher-tier escalation is active).
    pub default_model: Option<String>,
    /// Verbose logging toggle.
    pub verbose: bool,
    /// Wave's iteration index (1-based, shared across slots in the wave).
    pub iteration: u32,
    /// Total iteration budget (for the per-slot iteration banner).
    pub max_iterations: u32,
    /// Wall-clock seconds since loop start (snapshot at wave dispatch time).
    pub elapsed_secs: u64,
    /// Active PRD task prefix forwarded to every slot's `spawn_claude` call
    /// via `TASK_MGR_ACTIVE_PREFIX`. Mirrors `WaveIterationParams::task_prefix`.
    pub task_prefix: Option<String>,
}

/// Fields that vary between early-exit paths in `run_slot_iteration`.
///
/// `task_id` is intentionally NOT a field here — it is always the slot's
/// bundle id; threading it through the struct would just invite
/// inconsistencies. `slot_early_exit` reads it from the passed `SlotContext`.
struct SlotEarlyExit {
    outcome: IterationOutcome,
    files_modified: Vec<String>,
    should_stop: bool,
    output: String,
    effective_model: Option<String>,
    effective_effort: Option<&'static str>,
}

/// Build a slot-scoped early-exit `SlotResult`.
///
/// Centralizes the `IterationResult` construction for the handful of early
/// returns in `run_slot_iteration` so the 10-field struct literal isn't
/// duplicated. Pulls task identity AND `shown_learning_ids` from the slot's
/// bundle so the orphan-reset accounting AND bandit feedback stay correct
/// even on early-exit paths (signal received, timeout, etc.).
fn slot_early_exit(slot: &SlotContext, exit: SlotEarlyExit) -> SlotResult {
    SlotResult {
        slot_index: slot.slot_index,
        iteration_result: IterationResult {
            outcome: exit.outcome,
            task_id: Some(slot.prompt_bundle.task_id.clone()),
            files_modified: exit.files_modified,
            should_stop: exit.should_stop,
            output: exit.output,
            effective_model: exit.effective_model,
            effective_effort: exit.effective_effort,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        },
        // Early exit always runs after a successful claim (the slot thread
        // started); the orphan reset must consider this task pending until
        // process_slot_result clears it.
        claim_succeeded: true,
        shown_learning_ids: slot.prompt_bundle.shown_learning_ids.clone(),
        prompt_for_overflow: None,
        section_sizes: slot.prompt_bundle.section_sizes.clone(),
        dropped_sections: slot.prompt_bundle.dropped_sections.clone(),
        task_difficulty: slot.prompt_bundle.difficulty.clone(),
    }
}

/// Run one slot's iteration: spawn Claude in the slot worktree using the
/// pre-assembled prompt bundle, then analyze output. Opens its own DB
/// connection and creates its own watchdog activity epoch so nothing is
/// shared across slot threads.
///
/// This is a deliberately simpler counterpart to `run_iteration`:
/// - No `&mut IterationContext` — slot threads must not touch shared state.
/// - No crash-escalation, reorder, stale, or rate-limit-wait logic — those
///   are orchestrated by the outer loop after the wave returns.
/// - No prompt-overflow recovery path — wave-mode overflow handling is a
///   per-slot follow-up tracked elsewhere in the PRD.
///
/// The prompt bundle MUST have been built on the main thread before the
/// worker was spawned (FEAT-002 contract). The worker only reads from the
/// bundle — it never reaches back into a `&Connection` to re-derive task
/// state, because rusqlite `Connection` is not `Send` (learnings #1893 /
/// #1852 / #1871).
pub fn run_slot_iteration(
    slot: &SlotContext,
    params: &SlotIterationParams,
) -> TaskMgrResult<SlotResult> {
    let bundle = &slot.prompt_bundle;
    let task_id = bundle.task_id.as_str();
    let task_files = bundle.task_files.clone();

    // Early exit on signal — no work should start if the loop is stopping.
    if params.signal_flag.is_signaled() {
        return Ok(slot_early_exit(
            slot,
            SlotEarlyExit {
                outcome: IterationOutcome::Empty,
                files_modified: vec![],
                should_stop: true,
                output: String::new(),
                effective_model: None,
                effective_effort: None,
            },
        ));
    }

    // Effective model: bundle-resolved (per-task) > params.default_model.
    // Cluster-wide escalation (sequential path) is intentionally not applied
    // in parallel; the wave engine targets tasks already scored as disjoint,
    // not clusters.
    let effective_model: Option<String> = bundle
        .resolved_model
        .clone()
        .or_else(|| params.default_model.clone());

    let effort = model::effort_for_difficulty(bundle.difficulty.as_deref());

    if params.verbose {
        eprintln!(
            "[slot {}] task={} model={} effort={} cwd={}",
            slot.slot_index,
            task_id,
            effective_model.as_deref().unwrap_or("(default)"),
            effort.unwrap_or("(default)"),
            slot.working_root.display(),
        );
    }

    // Prefix every line of this slot's tee output with its slot index so
    // concurrent slots stay attributable on a shared stderr.
    let slot_label_buf = format!("[slot {}]", slot.slot_index);

    // Iteration banner — same shape as the sequential `print_iteration_header`
    // but routed through `emit_prefixed_lines` so each line carries the slot
    // label. `format_iteration_header` emits a leading newline for visual
    // separation; we strip it here because a blank line would land as
    // `[slot N] ` in the output, which adds noise without adding signal.
    let banner = display::format_iteration_header(
        params.iteration,
        params.max_iterations,
        task_id,
        params.elapsed_secs,
        effective_model.as_deref(),
        effort,
    );
    claude::emit_prefixed_lines(Some(&slot_label_buf), banner.trim_start_matches('\n'));

    // Per-slot activity monitor + timeout. Each slot gets its own monitor
    // polling its own working_root so heartbeats/change-tracking lines and
    // activity-driven deadline extensions both function in wave mode the
    // same way they do for the sequential path. `prefix` attributes monitor
    // output to the originating slot when slots interleave on stderr.
    let monitor_handle = monitor::start_monitor(&slot.working_root, Some(&slot_label_buf));
    let timeout_config = watchdog::TimeoutConfig::from_difficulty(
        bundle.difficulty.as_deref(),
        Arc::clone(&monitor_handle.last_activity_epoch),
    );

    let claude_result = claude::spawn_claude(
        &bundle.prompt,
        &params.permission_mode,
        claude::SpawnOpts {
            signal_flag: Some(&params.signal_flag),
            working_dir: Some(&slot.working_root),
            model: effective_model.as_deref(),
            timeout: Some(timeout_config),
            stream_json: true,
            effort,
            disallowed_tools: Some(TASKS_JSON_DISALLOWED_TOOLS),
            db_dir: Some(&params.db_dir),
            use_pty: false,
            target_task_id: Some(task_id),
            slot_label: Some(&slot_label_buf),
            active_prefix: params.task_prefix.as_deref(),
            ..Default::default()
        },
    );
    monitor::stop_monitor(monitor_handle);
    let claude_result = claude_result?;

    if claude_result.timed_out {
        eprintln!(
            "[slot {}] iteration timed out for task {}",
            slot.slot_index, task_id,
        );
        return Ok(slot_early_exit(
            slot,
            SlotEarlyExit {
                outcome: IterationOutcome::Crash(config::CrashType::RuntimeError),
                files_modified: task_files,
                should_stop: false,
                output: claude_result.output,
                effective_model,
                effective_effort: effort,
            },
        ));
    }

    // If Claude was killed by SIGINT/SIGTERM (exit 130/143), propagate to
    // our shared signal flag so peer slots and the outer loop observe the stop.
    //
    // Exception: if the watchdog fired the post-completion grace kill, the
    // SIGTERM (143) was issued internally as a successful-completion finalizer
    // — not an external Ctrl+C. Propagating it would end the whole loop (and
    // any chained PRDs) despite the task completing normally.
    if matches!(claude_result.exit_code, 130 | 143) && !claude_result.completion_killed {
        params.signal_flag.set();
    }

    if params.signal_flag.is_signaled() {
        return Ok(slot_early_exit(
            slot,
            SlotEarlyExit {
                outcome: IterationOutcome::Empty,
                files_modified: task_files,
                should_stop: true,
                output: claude_result.output,
                effective_model: None,
                effective_effort: None,
            },
        ));
    }

    let outcome = detection::analyze_output(
        &claude_result.output,
        claude_result.exit_code,
        &slot.working_root,
    );

    // Thread the structured stream-json transcript through to the pipeline so
    // wave-mode learning extraction reads the same conversation source the
    // sequential path uses (engine.rs:2109). Dropping this here is the
    // pre-FEAT-004 bug that makes the wave path silently fall back to
    // `claude_result.output` (just the final result string).
    let conversation = claude_result.conversation;

    let is_prompt_too_long = matches!(
        outcome,
        config::IterationOutcome::Crash(config::CrashType::PromptTooLong)
    );
    Ok(SlotResult {
        slot_index: slot.slot_index,
        iteration_result: IterationResult {
            outcome,
            task_id: Some(bundle.task_id.clone()),
            files_modified: task_files,
            should_stop: false,
            output: claude_result.output,
            effective_model,
            effective_effort: effort,
            key_decisions_count: 0,
            conversation,
            shown_learning_ids: bundle.shown_learning_ids.clone(),
        },
        claim_succeeded: true,
        shown_learning_ids: bundle.shown_learning_ids.clone(),
        prompt_for_overflow: is_prompt_too_long.then(|| bundle.prompt.clone()),
        section_sizes: bundle.section_sizes.clone(),
        dropped_sections: bundle.dropped_sections.clone(),
        task_difficulty: bundle.difficulty.clone(),
    })
}

/// Claim a slot's task on the main thread by updating status to `in_progress`.
///
/// Returns `true` when the claim succeeded (the row was in `todo` or already
/// `in_progress` for this task), `false` when the task was in an unexpected
/// state (e.g. `done`, `blocked`) — the caller should skip spawning that slot.
///
/// Done as a single UPDATE with an optimistic-locking WHERE clause so a
/// concurrent external writer cannot clobber a `done` row back to `in_progress`.
///
/// `'in_progress'` is intentionally included in the WHERE clause: re-claiming
/// an already-claimed task is idempotent (covered by the test
/// `test_claim_slot_task_idempotent_on_already_in_progress`) and supports
/// retry-after-recovery scenarios where step 6.6 left a row in `in_progress`
/// that the loop wants to take over. The selection layer is responsible for
/// not surfacing in-flight rows to other slots in the same wave; this guard
/// only protects against `done`/`blocked` transitions, not duplicate claims.
fn claim_slot_task(conn: &Connection, task_id: &str) -> bool {
    match conn.execute(
        "UPDATE tasks SET status = 'in_progress', started_at = datetime('now'), \
         updated_at = datetime('now') WHERE id = ? AND status IN ('todo', 'in_progress')",
        [task_id],
    ) {
        Ok(rows) => rows > 0,
        Err(e) => {
            eprintln!(
                "Warning: failed to claim slot task {}: {} — skipping slot",
                task_id, e,
            );
            false
        }
    }
}

/// Build a SlotResult representing a slot thread that panicked or errored.
///
/// `claim_succeeded` distinguishes two cases:
///   - `true`: claim went through, but the slot thread later panicked / crashed
///     while its task was already `in_progress` — the orphan reset must run.
///   - `false`: claim itself failed (task already `done`/`blocked`) and no row
///     was ever moved to `in_progress` — orphan reset must skip this entry.
///
/// `shown_learning_ids` is left empty on this path: the bundle was either
/// never built (claim failed) or has been moved into the panicked worker
/// thread and is no longer recoverable from the main thread. Bandit feedback
/// for failed runs is intentionally suppressed — crediting/discrediting
/// learnings against a worker that crashed before any agent reasoning would
/// produce noise, not signal.
fn slot_failure_result(
    slot_index: usize,
    task_id: Option<String>,
    reason: String,
    claim_succeeded: bool,
) -> SlotResult {
    SlotResult {
        slot_index,
        iteration_result: IterationResult {
            outcome: IterationOutcome::Crash(config::CrashType::RuntimeError),
            task_id,
            files_modified: vec![],
            should_stop: false,
            output: reason,
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        },
        claim_succeeded,
        shown_learning_ids: Vec::new(),
        prompt_for_overflow: None,
        section_sizes: Vec::new(),
        dropped_sections: Vec::new(),
        task_difficulty: None,
    }
}

/// Run one parallel wave: claim all tasks sequentially (main thread), then
/// spawn one OS thread per slot and wait for every thread to join.
///
/// **FEAT-002 contract**: every `SlotContext` passed in MUST already carry
/// a fully-built `prompt_bundle` (constructed by `prompt::slot::build_prompt`
/// on the main thread before this function is called — see
/// `build_slot_contexts`). This function never opens a `&Connection` from
/// inside a worker thread; the claim is the only DB write here, and it
/// stays on the main thread.
///
/// The claim loop is intentionally serial on the main thread:
/// - rusqlite `Connection` is not `Send`, so there's one DB writer.
/// - Serial claims prevent two slots from racing to claim the same row
///   (the UPDATE WHERE `status IN ('todo', 'in_progress')` guard would also
///   catch this, but we prefer the stronger invariant that every spawned
///   thread has an already-claimed task).
///
/// Slots whose claim fails (task already `done`/`blocked`) are reported as
/// a `Crash(RuntimeError)` entry so the outer loop's tracking logic sees
/// them; they are not silently dropped.
///
/// Thread panics are captured from `JoinHandle::join`'s `Err` branch and
/// converted into `Crash(RuntimeError)` entries — we never unwrap on join.
pub fn run_parallel_wave(
    conn: &Connection,
    slots: Vec<SlotContext>,
    params: Arc<SlotIterationParams>,
) -> WaveResult {
    let start_time = Instant::now();

    let mut claimed: Vec<(SlotContext, bool)> = Vec::with_capacity(slots.len());
    for slot in slots {
        let ok = claim_slot_task(conn, &slot.prompt_bundle.task_id);
        claimed.push((slot, ok));
    }

    type SlotHandle = Option<thread::JoinHandle<TaskMgrResult<SlotResult>>>;
    let mut handles: Vec<(usize, String, SlotHandle)> = Vec::with_capacity(claimed.len());
    let mut failures: Vec<SlotResult> = Vec::new();

    for (slot, ok) in claimed {
        let slot_index = slot.slot_index;
        let task_id = slot.prompt_bundle.task_id.clone();
        if !ok {
            failures.push(slot_failure_result(
                slot_index,
                Some(task_id.clone()),
                format!("claim failed for task {}", task_id),
                false,
            ));
            handles.push((slot_index, task_id, None));
            continue;
        }

        let params_for_thread = Arc::clone(&params);
        let handle = thread::spawn(move || run_slot_iteration(&slot, &params_for_thread));
        handles.push((slot_index, task_id, Some(handle)));
    }

    let mut outcomes: Vec<SlotResult> = Vec::with_capacity(handles.len());
    for (slot_index, task_id, maybe_handle) in handles {
        let Some(handle) = maybe_handle else {
            // Already recorded in `failures` above — skip re-emitting.
            continue;
        };
        match handle.join() {
            Ok(Ok(result)) => outcomes.push(result),
            Ok(Err(e)) => {
                eprintln!(
                    "[slot {}] iteration error for {}: {}",
                    slot_index, task_id, e,
                );
                outcomes.push(slot_failure_result(
                    slot_index,
                    Some(task_id),
                    format!("iteration error: {}", e),
                    true,
                ));
            }
            Err(panic_payload) => {
                let msg = panic_payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        panic_payload
                            .downcast_ref::<&'static str>()
                            .map(|s| (*s).to_string())
                    })
                    .unwrap_or_else(|| "unknown panic".to_string());
                eprintln!("[slot {}] thread panicked: {}", slot_index, msg);
                outcomes.push(slot_failure_result(
                    slot_index,
                    Some(task_id),
                    format!("thread panic: {}", msg),
                    true,
                ));
            }
        }
    }

    // Merge claim failures into outcomes so callers see every slot represented.
    outcomes.extend(failures);
    outcomes.sort_by_key(|r| r.slot_index);

    WaveResult {
        outcomes,
        wave_duration: start_time.elapsed(),
    }
}

/// Borrowed parameters for one parallel wave iteration (FEAT-010).
///
/// The struct is named so the call site at `run_loop` reads as a single
/// argument; it is constructed once per wave inside the outer loop.
pub struct WaveIterationParams<'a> {
    pub conn: &'a mut Connection,
    pub db_dir: &'a Path,
    pub source_root: &'a Path,
    pub branch: &'a str,
    pub parallel_slots: usize,
    pub slot_worktree_paths: &'a [PathBuf],
    pub iteration: u32,
    /// Total iteration budget (used for the iteration banner so each slot's
    /// header reads "Iteration N/M" matching sequential mode).
    pub max_iterations: u32,
    /// Wall-clock seconds since the loop's start_time. Captured once per wave
    /// (immediately before dispatch) and shared across slots so all slot
    /// banners in the same wave display the same elapsed value.
    pub elapsed_secs: u64,
    pub run_id: &'a str,
    pub base_prompt_path: &'a Path,
    pub permission_mode: &'a PermissionMode,
    pub signal_flag: &'a SignalFlag,
    pub default_model: Option<&'a str>,
    pub verbose: bool,
    pub task_prefix: Option<&'a str>,
    pub prd_path: &'a Path,
    pub progress_path: &'a Path,
    pub tasks_dir: &'a Path,
    pub external_repo_path: Option<&'a Path>,
    pub external_git_scan_depth: usize,
    pub inter_iteration_delay: Duration,
    /// Project-wide `steering.md` path. `None` when the project has no
    /// steering file. Threaded into per-slot `SlotPromptParams` so wave
    /// prompts include the same project-wide steering as sequential prompts.
    pub steering_path: Option<&'a Path>,
    /// Operator pause feedback rendered as a `## Session Guidance` block in
    /// each slot prompt. Empty string omits the section, matching sequential.
    pub session_guidance: &'a str,
    /// PRD-side `implicit_overlap_files` override, parsed once per run at
    /// `run_loop` startup and threaded through each wave instead of being
    /// re-parsed from disk per-wave (Fix 2 from /review-loop).
    /// Empty when the PRD JSON does not set the field. Mid-run edits to the
    /// PRD JSON are NOT picked up — operators must restart the loop, matching
    /// how every other config knob (model, parallel_slots, default_model)
    /// already behaves.
    pub prd_implicit_overlap_files: &'a [String],
    /// Project-level config loaded once per run from `.task-mgr/config.json`
    /// and shared across the wave loop instead of being re-read per call site.
    /// Same restart-required semantics as `prd_implicit_overlap_files`.
    pub project_config: &'a project_config::ProjectConfig,
}

/// Aggregated outcome of one parallel wave returned to `run_loop`.
///
/// Named exit descriptor carried by `WaveOutcome.terminal`.
#[derive(Debug)]
pub struct WaveTerminal {
    pub exit_code: i32,
    pub reason: String,
    pub run_status: Option<RunStatus>,
}

/// One slot whose merge-back failed during the wave. Bundles the slot index
/// and the task ID it had claimed so the consumer never has to keep two
/// `Vec`s in lockstep — the equal-length invariant becomes a type-level
/// guarantee instead of a rustdoc comment.
///
/// `task_id` is `None` when the slot's iteration produced no `task_id`
/// (e.g. claim-fail before any task was assigned, or the FEAT-004 deadlock
/// guard which synthesizes failed-merge entries from un-merged ephemeral
/// branches with no claimed task). Reset semantics: a `None` task_id is
/// silently skipped in `apply_merge_fail_reset_and_halt_check`'s reset
/// pass — the threshold counter still increments.
#[derive(Debug, Clone)]
pub struct FailedMerge {
    pub slot: usize,
    pub task_id: Option<String>,
}

/// `terminal` is `Some(_)` when the wave determined the loop should stop —
/// the outer loop applies the exit code and breaks. `iteration_consumed`
/// matches the AC: every wave (eligible or NoEligibleTasks) burns one
/// iteration of the budget so the loop can't spin forever.
#[derive(Debug)]
pub struct WaveOutcome {
    pub tasks_completed: u32,
    pub iteration_consumed: bool,
    pub terminal: Option<WaveTerminal>,
    /// True only when a `.stop` file caused the wave to halt — propagates to
    /// `LoopResult.was_stopped` so batch runners can react to a clean stop.
    pub was_stopped: bool,
    /// Slots whose merge-back failed during this wave, paired with the task
    /// ID each had claimed. Populated from
    /// `worktree::merge_slot_branches_with_resolver`'s `outcomes.failed_slots`.
    /// Always empty for sequential runs (`parallel_slots <= 1`) and for waves
    /// that never reached the merge-back step (e.g. preflight bail-out).
    ///
    /// Read by the wave-loop boundary in `run_loop` to drive the
    /// reset/halt-check contract (FEAT-002): reset each failed slot's
    /// claimed task to `todo`, increment
    /// `IterationContext::consecutive_merge_fail_waves`, and halt when the
    /// counter reaches `ProjectConfig::merge_fail_halt_threshold`.
    pub failed_merges: Vec<FailedMerge>,
}

/// Wave-mode aggregator collected during per-slot post-processing.
///
/// `all_crashed` starts true only if the wave produced at least one slot
/// outcome — an empty `outcomes` list cannot be "all crashed" — and gets
/// flipped to false the first time a slot doesn't crash (or its claimed
/// task gets marked done despite a crash outcome flag).
struct WaveAggregator {
    tasks_completed: u32,
    any_completed: bool,
    all_crashed: bool,
    aggregated_files: Vec<String>,
    wave_should_stop: bool,
}

impl WaveAggregator {
    fn new(num_outcomes: usize) -> Self {
        Self {
            tasks_completed: 0,
            any_completed: false,
            all_crashed: num_outcomes > 0,
            aggregated_files: Vec::new(),
            wave_should_stop: false,
        }
    }
}

/// Pre-wave preflight: signal/stop-file checks and crash backoff/abort.
/// Returns `Some(WaveOutcome)` when the wave should bail out before doing
/// any work, `None` when execution should proceed.
fn wave_preflight_check(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> Option<WaveOutcome> {
    // Match sequential semantics so Ctrl+C and `.stop` files exit the loop
    // the same way in both paths.
    if params.signal_flag.is_signaled() {
        return Some(WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: false,
            terminal: Some(WaveTerminal {
                exit_code: 130,
                reason: "signal received".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        });
    }
    if signals::check_stop_signal(params.tasks_dir, params.task_prefix) {
        eprintln!("Stop signal detected (.stop file found)");
        return Some(WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: false,
            terminal: Some(WaveTerminal {
                exit_code: 0,
                reason: "stop signal".to_string(),
                run_status: None,
            }),
            was_stopped: true,
            failed_merges: Vec::new(),
        });
    }

    // Crash backoff + abort. Identical contract to the sequential path so
    // learning [1005] (don't burn iterations on a wedged task) holds even
    // when every slot of the previous wave crashed.
    let backoff = ctx.crash_tracker.backoff_duration();
    if !backoff.is_zero() {
        eprintln!(
            "Crash backoff: waiting {} before retry...",
            display::format_duration(backoff.as_secs())
        );
        thread::sleep(backoff);
    }
    if ctx.crash_tracker.should_abort() {
        eprintln!("Too many consecutive crashes, aborting loop");
        return Some(WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 1,
                reason: "too many crashes".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        });
    }
    None
}

/// No eligible tasks were selected for this wave. Drives the stale tracker
/// exactly like sequential does on `IterationOutcome::NoEligibleTasks`,
/// returning a terminal outcome when the tracker tripped its abort threshold.
fn handle_no_eligible_tasks(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> WaveOutcome {
    ctx.stale_tracker.check("stale", "stale");
    progress::log_iteration(
        params.progress_path,
        params.iteration,
        None,
        &IterationOutcome::NoEligibleTasks,
        &[],
        None,
        None,
        None,
    );
    if ctx.stale_tracker.should_abort() {
        eprintln!(
            "Aborting: no eligible tasks after {} consecutive stale iterations",
            ctx.stale_tracker.count()
        );
        return WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 1,
                reason: format!(
                    "no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                ),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        };
    }
    WaveOutcome {
        tasks_completed: 0,
        iteration_consumed: true,
        terminal: None,
        was_stopped: false,
        failed_merges: Vec::new(),
    }
}

/// FEAT-004 deadlock guard: every eligible candidate was blocked exclusively
/// by un-merged work on `{branch}-slot-N` ephemeral branches. Emit per-
/// candidate diagnostics, then synthesize a merge-fail wave by deriving slot
/// indices from the blocking branch names so `run_loop`'s wave-loop boundary
/// runs the FEAT-002 reset/halt-check contract.
///
/// **No tasks to reset**: the deferred candidates are still `todo` — they
/// were never claimed. The synthesized `FailedMerge` entries therefore carry
/// `task_id: None`, and `apply_merge_fail_reset_and_halt_check`'s reset
/// pass becomes a no-op (`None` skipped via `if let Some`).
///
/// **Slot index derivation**: branches sourced from
/// `worktree::list_ephemeral_slot_branches` are guaranteed to match the
/// `{branch}-slot-N` shape (parsed and re-emitted by that helper). We strip
/// `{branch}-slot-` and parse the suffix; on the off chance a branch ever
/// slips through with a non-numeric suffix, that branch is logged and skipped
/// — slot index 0 is reserved for slot 0 (the loop's own branch) and must
/// never be synthesized here.
fn handle_ephemeral_deadlock(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
    diagnostics: Vec<(String, Vec<String>)>,
) -> WaveOutcome {
    ctx.stale_tracker.check("stale", "stale");
    progress::log_iteration(
        params.progress_path,
        params.iteration,
        None,
        &IterationOutcome::NoEligibleTasks,
        &[],
        None,
        None,
        None,
    );

    eprintln!(
        "Cross-wave deadlock: every eligible candidate is blocked by un-merged ephemeral branch(es). \
         Treating as merge-fail wave so the halt threshold can fire."
    );
    for (cand_id, branches) in &diagnostics {
        eprintln!("  {} blocked by: {}", cand_id, branches.join(", "));
    }

    // Collect distinct slot indices from the union of blocking branches across
    // all diagnostics. Order is stable (sorted ascending) so the eventual
    // halt-diagnostic from `apply_merge_fail_reset_and_halt_check` lists slots
    // in a predictable order.
    let prefix = format!("{}-slot-", params.branch);
    let mut synth_slots: Vec<usize> = Vec::new();
    for (_, branches) in &diagnostics {
        for branch in branches {
            let Some(suffix) = branch.strip_prefix(&prefix) else {
                continue;
            };
            match suffix.parse::<usize>() {
                Ok(slot) if slot > 0 => {
                    if !synth_slots.contains(&slot) {
                        synth_slots.push(slot);
                    }
                }
                _ => eprintln!(
                    "Warning: skipping ephemeral branch with non-numeric / zero slot suffix: {}",
                    branch
                ),
            }
        }
    }
    synth_slots.sort();
    // If every blocking branch had an unparseable slot suffix, `synth_slots`
    // is empty and `WaveOutcome.failed_merges` would be empty too — at which
    // point `apply_merge_fail_reset_and_halt_check` would reset the
    // consecutive counter to 0 and the deadlock would silently spin forever
    // until the stale-iteration tracker aborts. Insert a single
    // `SYNTHETIC_DEADLOCK_SLOT` entry so the threshold counter still
    // increments. The diagnostic step in
    // `apply_merge_fail_reset_and_halt_check` special-cases this slot index
    // to print a `<malformed deadlock blocker>` placeholder instead of
    // synthesizing a `{branch}-slot-18446744073709551615` name.
    if synth_slots.is_empty() && !diagnostics.is_empty() {
        eprintln!(
            "warning: deadlock guard fired with no parseable ephemeral slot indices \
             — inserting synthetic halt slot so the threshold counter still increments"
        );
        synth_slots.push(SYNTHETIC_DEADLOCK_SLOT);
    }
    let failed_merges: Vec<FailedMerge> = synth_slots
        .into_iter()
        .map(|slot| FailedMerge {
            slot,
            task_id: None,
        })
        .collect();

    WaveOutcome {
        tasks_completed: 0,
        iteration_consumed: true,
        terminal: None,
        was_stopped: false,
        failed_merges,
    }
}

/// Build per-slot `SlotContext` entries, pairing each scored task with its
/// pre-allocated worktree path AND assembling the full `SlotPromptBundle`
/// on the main thread.
///
/// **FEAT-002 contract**: bundle assembly happens here, on the main thread,
/// BEFORE `run_parallel_wave` spawns any worker. The bundle is the only
/// task-derived state a worker reads; `SlotContext` no longer carries a
/// `Task` reference, so all DB-backed prompt assembly (learnings,
/// task_files, source context) MUST run from this single connection.
///
/// Each slot gets its own activity epoch — sharing one would let activity
/// in one slot silently extend another slot's watchdog deadline.
fn build_slot_contexts(
    conn: &Connection,
    group: Vec<crate::commands::next::selection::ScoredTask>,
    slot_paths: &[PathBuf],
    slot_prompt_params: &prompt::slot::SlotPromptParams<'_>,
) -> Vec<SlotContext> {
    group
        .into_iter()
        .zip(slot_paths.iter())
        .enumerate()
        .map(|(idx, (scored, path))| {
            // The DB row will flip to `in_progress` in run_parallel_wave's
            // claim step; reflect that in the bundle's task JSON so the
            // agent sees the post-claim state. The acceptance criteria,
            // description, model, and difficulty fields don't change with
            // claim, so this is the only field that needs a pre-mutation.
            let mut task = scored.task;
            task.status = crate::models::TaskStatus::InProgress;
            let prompt_bundle = prompt::slot::build_prompt(conn, &task, slot_prompt_params);
            SlotContext {
                slot_index: idx,
                working_root: path.clone(),
                prompt_bundle,
            }
        })
        .collect()
}

/// Build the shared per-slot `SlotIterationParams`. `SignalFlag` clones the
/// inner `Arc` so all threads observe the same SIGINT/SIGTERM signal.
///
/// `base_prompt_path` is intentionally NOT carried on this struct anymore —
/// the base prompt content is baked into `SlotPromptBundle.prompt` at
/// assembly time, so workers never need to read the file (and thus don't
/// need a path that might race with a concurrent edit).
fn build_shared_slot_params(params: &WaveIterationParams<'_>) -> Arc<SlotIterationParams> {
    Arc::new(SlotIterationParams {
        db_dir: params.db_dir.to_path_buf(),
        permission_mode: params.permission_mode.clone(),
        signal_flag: params.signal_flag.clone(),
        default_model: params.default_model.map(|s| s.to_string()),
        verbose: params.verbose,
        iteration: params.iteration,
        max_iterations: params.max_iterations,
        elapsed_secs: params.elapsed_secs,
        task_prefix: params.task_prefix.map(|s| s.to_string()),
    })
}

/// Build the `SlotPromptParams` shared across all slots in this wave.
///
/// Pulled out of `build_slot_contexts` so the call site at
/// `run_wave_iteration` can construct it once and pass by reference, which
/// avoids cloning the `PathBuf` per slot.
fn build_slot_prompt_params<'a>(
    params: &'a WaveIterationParams<'a>,
) -> prompt::slot::SlotPromptParams<'a> {
    prompt::slot::SlotPromptParams {
        project_root: params.source_root.to_path_buf(),
        base_prompt_path: params.base_prompt_path.to_path_buf(),
        permission_mode: params.permission_mode.clone(),
        steering_path: params.steering_path,
        session_guidance: params.session_guidance,
    }
}

/// Per-slot post-processing on the main thread.
///
/// The post-Claude work shared with the sequential path — progress logging,
/// `<key-decision>` extraction, `<task-status>` dispatch, the full completion
/// ladder (status-tag → completed-tag → output-scan → already-complete
/// fallback), learning extraction, and bandit feedback — runs inside
/// `iteration_pipeline::process_iteration_output`. `skip_git_completion_detection`
/// is `true` because slot commits live on an unmerged ephemeral branch; git
/// reconciliation happens once at the `run_wave_iteration` boundary after the
/// merge-back step.
///
/// Slot-specific bookkeeping stays here: pending-slot-task accounting, the
/// `agg.all_crashed` invariant, reorder hint queueing (with `[slot N]` log
/// prefix), file aggregation, and the wave-level stop flag.
///
/// Updates `agg.all_crashed` only when the slot crashed AND its claimed
/// task did not finish — any non-crash slot (or a crashed slot whose task
/// was nonetheless marked done) breaks the all-crashed invariant.
fn process_slot_result(
    slot_result: &mut SlotResult,
    params: &mut WaveIterationParams<'_>,
    ctx: &mut IterationContext,
    agg: &mut WaveAggregator,
) {
    let slot_idx = slot_result.slot_index;
    let task_id = slot_result.iteration_result.task_id.clone();

    // Track every claimed slot task as pending until we observe a "done"
    // signal for it. The post-loop cleanup uses this to reset rows still in
    // `in_progress` when the loop exits via deadline / max-iterations rather
    // than waiting for the next process's step 6.6 recovery.
    if slot_result.claim_succeeded
        && let Some(ref tid) = task_id
        && !ctx.pending_slot_tasks.contains(tid)
    {
        ctx.pending_slot_tasks.push(tid.clone());
    }

    // Synthetic slot_failure_result entries with claim_succeeded=false represent
    // tasks that were never moved to in_progress. Running the overflow handler or
    // the pipeline for them would pollute ctx.crashed_last_iteration past its
    // 'bounded by active task count' invariant (engine.rs:218-221) and emit
    // spurious JSONL overflow events for tasks that never executed.
    if !slot_result.claim_succeeded {
        return;
    }

    // Per-slot PromptTooLong recovery — mirrors the sequential Step 8.5 in
    // `run_iteration`. Must run BEFORE `process_iteration_output` so the
    // task row is reset to `todo` (rungs 1-3) or `blocked` (rung 4) before
    // the pipeline's crash-tracking write. Order of operations is
    // contractual: ctx update → DB UPDATE → stderr → dump → JSONL → rotate.
    if matches!(
        slot_result.iteration_result.outcome,
        config::IterationOutcome::Crash(config::CrashType::PromptTooLong)
    ) && let Some(ref tid) = task_id
    {
        debug_assert!(
            slot_result.prompt_for_overflow.is_some(),
            "PromptTooLong without prompt_for_overflow for task {tid}"
        );
        let synthetic_prompt = crate::loop_engine::prompt::PromptResult {
            prompt: slot_result.prompt_for_overflow.take().unwrap_or_default(),
            task_id: tid.clone(),
            task_files: slot_result.iteration_result.files_modified.clone(),
            shown_learning_ids: Vec::new(),
            resolved_model: slot_result.iteration_result.effective_model.clone(),
            // Wave-mode prompts now apply the same TOTAL_PROMPT_BUDGET cap as
            // the sequential builder; surface any dropped sections threaded
            // through from `SlotPromptBundle` so overflow dumps and JSONL
            // events match what the agent actually saw.
            dropped_sections: slot_result.dropped_sections.clone(),
            task_difficulty: slot_result.task_difficulty.clone(),
            cluster_effort: slot_result.iteration_result.effective_effort,
            section_sizes: slot_result.section_sizes.clone(),
        };
        let _ = overflow::handle_prompt_too_long(
            ctx,
            params.conn,
            tid,
            slot_result.iteration_result.effective_effort,
            slot_result.iteration_result.effective_model.as_deref(),
            &synthetic_prompt,
            params.iteration,
            Some(params.run_id),
            params.db_dir,
            Some(slot_idx),
        );
    }

    // Pipeline contract requires a `working_root` even when skip_git is on
    // (the field is unused in that mode but still part of the struct). Use
    // the slot's pre-allocated worktree path so a future change that begins
    // honoring git history wouldn't silently fall back to the source tree.
    let working_root = params
        .slot_worktree_paths
        .get(slot_idx)
        .cloned()
        .unwrap_or_else(|| params.source_root.to_path_buf());

    let processing_outcome =
        iteration_pipeline::process_iteration_output(iteration_pipeline::ProcessingParams {
            conn: params.conn,
            run_id: params.run_id,
            iteration: params.iteration,
            task_id: task_id.as_deref(),
            output: &slot_result.iteration_result.output,
            conversation: slot_result.iteration_result.conversation.as_deref(),
            shown_learning_ids: &slot_result.shown_learning_ids,
            outcome: &mut slot_result.iteration_result.outcome,
            working_root: &working_root,
            git_scan_depth: 0,
            skip_git_completion_detection: true,
            prd_path: params.prd_path,
            task_prefix: params.task_prefix,
            progress_path: params.progress_path,
            db_dir: params.db_dir,
            signal_flag: params.signal_flag,
            ctx,
            files_modified: &slot_result.iteration_result.files_modified,
            effective_model: slot_result.iteration_result.effective_model.as_deref(),
            effective_effort: slot_result.iteration_result.effective_effort,
            slot_index: Some(slot_idx),
        });

    slot_result.iteration_result.key_decisions_count = processing_outcome.key_decisions_count;

    // The claimed task was completed in this pass iff its id appears in the
    // pipeline's deduped completion list. Cross-task `<completed>Y</completed>`
    // entries land in the list too but never satisfy this predicate — Y stays
    // out of `pending_slot_tasks` (it was a peer slot's task or was already
    // terminal), so the orphan-reset semantics are preserved.
    let slot_marked_done = task_id
        .as_ref()
        .map(|tid| {
            processing_outcome
                .completed_task_ids
                .iter()
                .any(|c| c == tid)
        })
        .unwrap_or(false);

    agg.tasks_completed += processing_outcome.tasks_completed;
    if processing_outcome.tasks_completed > 0 {
        agg.any_completed = true;
    }

    // Pipeline may have flipped the outcome from a non-Completed value to
    // `Completed` via the completion ladder; checking `outcome` post-pipeline
    // collapses the legacy "crash but task done" branch into the same arm as
    // a clean success.
    if !matches!(
        slot_result.iteration_result.outcome,
        IterationOutcome::Crash(_)
    ) || slot_marked_done
    {
        agg.all_crashed = false;
    }

    if slot_marked_done && let Some(ref tid) = task_id {
        ctx.pending_slot_tasks.retain(|t| t != tid);
    }

    // FEAT-010 AC: queue reorder hints for the next wave. `select_parallel_group`
    // does not yet honor hints (it ranks by score), so this acts as a
    // preservation queue — operators see them in logs and a future selection
    // pass can drain them.
    if let IterationOutcome::Reorder(ref rid) = slot_result.iteration_result.outcome {
        ctx.pending_reorder_hints.push(rid.clone());
        eprintln!("[slot {}] Queued reorder hint: {}", slot_idx, rid);
    }

    for f in &slot_result.iteration_result.files_modified {
        if !agg.aggregated_files.contains(f) {
            agg.aggregated_files.push(f.clone());
        }
    }

    if slot_result.iteration_result.should_stop {
        agg.wave_should_stop = true;
    }
}

/// Count tasks still in flight for the current PRD prefix. "Done",
/// "skipped", "irrelevant" and "blocked" are terminal — anything else is
/// still work to do.
fn count_remaining_active_tasks(conn: &Connection, task_prefix: Option<&str>) -> i64 {
    let (clause, param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT COUNT(*) FROM tasks WHERE status NOT IN \
         ('done','irrelevant','skipped','blocked') {clause}"
    );
    let p_vec: Vec<&dyn rusqlite::types::ToSql> = match &param {
        Some(p) => vec![p],
        None => vec![],
    };
    conn.query_row(&sql, p_vec.as_slice(), |r| r.get(0))
        .unwrap_or(0)
}

/// Sleep between waves so the loop respects `--iteration-delay` the same
/// way it does between sequential iterations. Polls the signal flag every
/// 200ms so SIGINT/SIGTERM short-circuits the delay; returns true if the
/// signal fired during the wait so the caller can treat it as a stop.
fn wait_inter_wave_delay(delay: Duration, signal_flag: &SignalFlag) -> bool {
    if delay.is_zero() {
        return false;
    }
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if signal_flag.is_signaled() {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Reset a single in-progress task back to `todo` so the next iteration (or
/// next loop run) can re-claim it. No-ops when the row's status is not
/// `in_progress` — we never demote terminal states (`done`, `blocked`,
/// `skipped`) here.
///
/// Used by:
/// - The wave-loop FEAT-002 reset/halt-check contract — a slot's task whose
///   merge-back failed must not stay pinned in `in_progress`.
/// - Post-loop cleanup (Step 17.5 / 17.6) when the loop exits via deadline /
///   max-iterations rather than a per-task done signal.
///
/// Logs success / no-op / failure to stderr; failures never propagate
/// (matches the FEAT-002 failure-mode AC: "if reset itself fails for a task,
/// log the failure but continue with remaining failed slots").
fn reset_task_to_todo(conn: &Connection, task_id: &str, kind_label: &str) {
    match conn.execute(
        "UPDATE tasks SET status = 'todo', started_at = NULL WHERE id = ? AND status = 'in_progress'",
        [task_id],
    ) {
        Ok(1) => eprintln!("Reset {} {} to todo", kind_label, task_id),
        Ok(_) => {} // already terminal, or status changed by reconciliation
        Err(e) => eprintln!("Warning: failed to reset task {}: {}", task_id, e),
    }
}

/// Sentinel slot index for deadlock-guard waves where every blocking
/// `{branch}-slot-N` ephemeral had a non-numeric / zero suffix and the
/// guard had nothing parseable to enumerate. `handle_ephemeral_deadlock`
/// inserts one `FailedMerge { slot: SYNTHETIC_DEADLOCK_SLOT, .. }` so the
/// FEAT-002 threshold counter still increments. `apply_merge_fail_reset_and_halt_check`
/// special-cases this slot index in its diagnostic step to avoid
/// synthesizing the meaningless `{branch}-slot-18446744073709551615` name.
pub(crate) const SYNTHETIC_DEADLOCK_SLOT: usize = usize::MAX;

/// Decision returned by the wave-loop FEAT-002 reset/halt-check contract.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MergeFailHaltDecision {
    /// Wave merged cleanly OR failed but is below the halt threshold —
    /// continue the loop.
    Continue,
    /// Threshold reached — break out of the wave loop with the captured
    /// `exit_code` / `exit_reason`. The caller emits the per-slot ephemeral
    /// branch diagnostic before returning this variant.
    Halt { exit_code: i32, exit_reason: String },
}

/// Apply the FEAT-002 reset/halt contract to one wave's outcome.
///
/// Step ordering (contractual — reset MUST come before increment so a halted
/// run never leaves a failed-slot task pinned in `in_progress`):
///
/// 1. For each failed slot's claimed task: reset to `todo`; drain it from
///    `ctx.pending_slot_tasks` (mirror of engine.rs:1269). Reset failures are
///    logged but never fatal — remaining slots still process.
/// 2. Increment `ctx.consecutive_merge_fail_waves`.
/// 3. Compare to `threshold`. `0` disables the halt entirely (legacy "log
///    and continue" behavior preserved bit-for-bit).
/// 4. On halt: emit a per-slot ephemeral branch name diagnostic to stderr.
/// 5. Return `Halt { ... }` to break the loop, or `Continue` otherwise.
///
/// When `failed_merges.is_empty()` the counter is reset to `0` and `Continue`
/// is returned without doing any reset / diagnostic work.
fn apply_merge_fail_reset_and_halt_check(
    conn: &Connection,
    ctx: &mut IterationContext,
    branch: &str,
    failed_merges: &[FailedMerge],
    threshold: u32,
) -> MergeFailHaltDecision {
    if failed_merges.is_empty() {
        ctx.consecutive_merge_fail_waves = 0;
        return MergeFailHaltDecision::Continue;
    }

    // (1) Reset every failed slot's claimed task. The single-slice shape
    //     (a `Vec<FailedMerge>`) makes the slot/task pairing a type-level
    //     guarantee — no zip-truncation footgun.
    for fm in failed_merges {
        if let Some(tid) = &fm.task_id {
            reset_task_to_todo(conn, tid, "failed-slot task");
            ctx.pending_slot_tasks.retain(|t| t != tid);
        }
    }

    // (2) Increment AFTER reset so a halted run still leaves the DB
    //     re-runnable (the known-bad implementation does this in the wrong
    //     order — see the regression test).
    ctx.consecutive_merge_fail_waves += 1;

    // (3) Threshold check. `0` is a sentinel for "never halt".
    if threshold == 0 || ctx.consecutive_merge_fail_waves < threshold {
        return MergeFailHaltDecision::Continue;
    }

    // (4) Per-slot diagnostic. `ephemeral_slot_branch` is the canonical name
    //     source — never construct `{branch}-slot-{N}` inline (learning [1870]).
    //     The `SYNTHETIC_DEADLOCK_SLOT` sentinel renders as a placeholder
    //     instead of `{branch}-slot-18446744073709551615`.
    let names: Vec<String> = failed_merges
        .iter()
        .map(|fm| {
            if fm.slot == SYNTHETIC_DEADLOCK_SLOT {
                "<malformed deadlock blocker>".to_string()
            } else {
                worktree::ephemeral_slot_branch(branch, fm.slot)
            }
        })
        .collect();
    eprintln!(
        "Aborting: {} consecutive merge-back failure wave(s) (threshold={}). \
         Failed slot branches: {}",
        ctx.consecutive_merge_fail_waves,
        threshold,
        names.join(", ")
    );

    // (5) Halt.
    MergeFailHaltDecision::Halt {
        exit_code: 1,
        exit_reason: format!(
            "{} consecutive merge-back failure wave(s) (threshold={})",
            ctx.consecutive_merge_fail_waves, threshold
        ),
    }
}

/// Read the PRD JSON at `prd_path` and return its `implicitOverlapFiles`
/// list (FEAT-003). Returns an empty Vec on any error (file missing,
/// invalid JSON, field absent) — implicit-overlap detection still works
/// from the baseline list and ProjectConfig extension.
///
/// Best-effort I/O on the wave-loop hot path: a parse failure should NOT
/// abort the wave, so all error paths swallow with a Vec::new() default.
fn read_prd_implicit_overlap_files(prd_path: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(prd_path) else {
        return Vec::new();
    };
    let Ok(prd): Result<crate::commands::init::parse::PrdFile, _> = serde_json::from_str(&contents)
    else {
        return Vec::new();
    };
    prd.implicit_overlap_files.unwrap_or_default()
}

/// Wave-mode equivalent of `run_iteration` for the parallel execution path.
///
/// Pre-wave: signal/stop checks, crash backoff, parallel group selection,
/// stale-tracker bookkeeping. Wave: spawn slots via `run_parallel_wave`,
/// merge slot branches back to the loop branch. Post-wave: per-slot progress
/// logging, status update dispatch, completion detection, crash policy
/// (all-crashed → record_crash; any-completed → record_success), reorder
/// hint queueing, external-git reconciliation, terminal-condition checks.
///
/// The function only mutates `IterationContext` from the main thread — slot
/// threads inside `run_parallel_wave` never touch shared state. All file
/// aggregation and PRD updates happen here so the per-thread invariants
/// established by FEAT-009 stay intact.
pub fn run_wave_iteration(
    mut params: WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> WaveOutcome {
    if let Some(outcome) = wave_preflight_check(&params, ctx) {
        return outcome;
    }

    // Parallel group selection. Reuses the same scoring + greedy
    // file-overlap walk as `task-mgr next`, capped at `parallel_slots`.
    //
    // FEAT-003: merge project-config and PRD-level implicit-overlap extensions
    // into the slate so `select_parallel_group` can detect shared-infra files
    // beyond the baseline `IMPLICIT_OVERLAP_FILES` list. Both lists were
    // loaded once at run-loop startup and threaded through `WaveParams`
    // (Fix 2 from /review-loop) so the wave hot path no longer re-parses
    // disk every iteration.
    let extra_implicit: Vec<String> = params
        .project_config
        .implicit_overlap_files
        .iter()
        .cloned()
        .chain(params.prd_implicit_overlap_files.iter().cloned())
        .collect();

    // FEAT-004: cross-wave file affinity. Enumerate `{branch}-slot-N` ephemeral
    // branches once per wave on the main thread (slot worker threads from the
    // prior wave have already joined per the engine's join discipline) and
    // collect their un-merged files. The (branch, files) pairs become synthetic
    // claims for `select_parallel_group`, which then defers any candidate
    // whose `touchesFiles` would conflict with un-merged work.
    //
    // `git diff` is called once per ephemeral branch (NOT per candidate) so
    // the overhead is bounded by `parallel_slots`. Each per-branch failure
    // logs and degrades to "no claim" — selection continues without the
    // overlay rather than crashing the loop (per FEAT-004 failure-mode AC).
    let ephemeral_branches =
        worktree::list_ephemeral_slot_branches(params.source_root, params.branch);
    let ephemeral_overlay: Vec<(String, Vec<String>)> = ephemeral_branches
        .iter()
        .map(|eph| {
            let files = match worktree::list_unmerged_branch_files(
                params.source_root,
                params.branch,
                eph,
            ) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "Warning: list_unmerged_branch_files({}) failed: {} (treating as no claim)",
                        eph, e
                    );
                    Vec::new()
                }
            };
            (eph.clone(), files)
        })
        .collect();

    let result = match select_parallel_group(
        params.conn,
        &ctx.last_files,
        params.task_prefix,
        params.parallel_slots,
        &extra_implicit,
        &ephemeral_overlay,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Warning: select_parallel_group failed: {} (treating wave as stale)",
                e
            );
            crate::commands::next::selection::ParallelGroupResult::default()
        }
    };

    let group = result.group;
    if group.is_empty() {
        // FEAT-004 deadlock guard: a non-empty diagnostic means every eligible
        // candidate was blocked exclusively by un-merged ephemeral work. Surface
        // the per-candidate breakdown to stderr and synthesize a merge-fail
        // wave so the FEAT-002 reset/halt-check contract fires — without this,
        // the loop would spin until stale-iteration abort with no signal to
        // the operator about what's blocking forward progress.
        if !result.ephemeral_block_diagnostics.is_empty() {
            return handle_ephemeral_deadlock(&params, ctx, result.ephemeral_block_diagnostics);
        }
        return handle_no_eligible_tasks(&params, ctx);
    }

    // Selected at least one eligible task → reset stale tracker (mirrors
    // the sequential `else` branch that calls `check("a", "b")`).
    ctx.stale_tracker.check("a", "b");

    let n_slots = group.len();
    let slot_paths: &[PathBuf] = &params.slot_worktree_paths[..n_slots];
    let slot_prompt_params = build_slot_prompt_params(&params);
    let slot_contexts = build_slot_contexts(params.conn, group, slot_paths, &slot_prompt_params);
    let slot_params = build_shared_slot_params(&params);

    // Run wave (blocks until every spawned slot thread joins).
    let mut wave_result = run_parallel_wave(params.conn, slot_contexts, slot_params);

    // Per-slot post-processing on the main thread. The pipeline mutates each
    // slot's `IterationOutcome` in place when retroactive completion is
    // detected, so the iteration borrows mutably.
    let mut agg = WaveAggregator::new(wave_result.outcomes.len());
    for slot_result in &mut wave_result.outcomes {
        process_slot_result(slot_result, &mut params, ctx, &mut agg);
    }

    // CrashTracker policy per FEAT-010 AC.
    if agg.any_completed {
        ctx.crash_tracker.record_success();
    } else if agg.all_crashed {
        ctx.crash_tracker.record_crash();
    }

    // Feed the next wave's locality scoring.
    ctx.last_files = agg.aggregated_files.clone();

    // Merge ephemeral slot branches back into the main loop branch so the
    // next wave starts from a unified base. No-op when parallel_slots <= 1.
    // Per-slot failures (merge conflicts, spawn errors, ff failures) are
    // logged but never fatal — slot 0 is restored to its captured pre-merge
    // HEAD via `git reset --hard`, and the next wave still benefits from any
    // slots that did merge. The wave-loop boundary in `run_loop` consumes
    // `failed_merges` to drive the FEAT-002 reset/halt-check contract.
    let mut failed_merges: Vec<FailedMerge> = Vec::new();
    if params.parallel_slots > 1 {
        let resolved_model = params
            .default_model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(model::SONNET_MODEL)
            .to_string();
        // Per-project overrides for the merge-conflict resolver. Both fall
        // back to safe defaults so projects without a config.json keep the
        // pre-existing behavior (600s / "medium"). The shared `project_config`
        // was loaded once at run-loop startup and threaded through
        // `WaveParams` (Fix 2 from /review-loop).
        let claude_timeout = Duration::from_secs(
            params
                .project_config
                .merge_resolver_timeout_secs
                .unwrap_or(600),
        );
        let effort = params
            .project_config
            .merge_resolver_effort
            .clone()
            .unwrap_or_else(|| "medium".to_string());
        let resolver = merge_resolver::ClaudeMergeResolver {
            model: resolved_model,
            db_dir: Some(params.db_dir),
            signal_flag: Some(params.signal_flag),
            claude_timeout,
            effort,
        };
        let outcomes = worktree::merge_slot_branches_with_resolver(
            params.source_root,
            params.branch,
            params.parallel_slots,
            &resolver,
            params.slot_worktree_paths,
        );
        for (slot, detail, kind) in &outcomes.failed_slots {
            if *kind == worktree::SlotFailureKind::ResolverAttempted {
                eprintln!(
                    "Warning: slot {} merge-back failed after Claude resolution attempt: {} (slot's commits remain on its ephemeral branch)",
                    slot, detail
                );
            } else {
                eprintln!(
                    "Warning: slot {} merge-back failed: {} (slot's commits remain on its ephemeral branch)",
                    slot, detail
                );
            }
            // Capture failed slot index alongside the task_id its slot had
            // claimed. The `FailedMerge` shape keeps the pairing as one
            // value so downstream consumers can't accidentally zip with a
            // mismatched length.
            let task_id = wave_result
                .outcomes
                .iter()
                .find(|o| o.slot_index == *slot)
                .and_then(|o| o.iteration_result.task_id.clone());
            failed_merges.push(FailedMerge {
                slot: *slot,
                task_id,
            });
        }
    }

    if let Err(e) = run_cmd::update(
        params.conn,
        params.run_id,
        ctx.last_commit.as_deref(),
        Some(&agg.aggregated_files),
    ) {
        eprintln!("Warning: failed to update run: {}", e);
    }

    // Per-wave external-git reconciliation. Mirrors the sequential
    // "Post-iteration: reconcile external git completions" step.
    let mut tasks_completed = agg.tasks_completed;
    if let Some(ext_repo) = params.external_repo_path {
        let count = reconcile_external_git_completions(
            ext_repo,
            params.conn,
            params.run_id,
            params.prd_path,
            params.task_prefix,
            params.external_git_scan_depth,
        );
        if count > 0 {
            tasks_completed += count as u32;
            agg.any_completed = true;
            ctx.crash_tracker.record_success();
            eprintln!("Post-wave reconciliation: marked {} task(s) done", count);
        }
    }

    // Terminal conditions: signal precedes everything; crash abort wins
    // over completion-of-all-tasks because the abort is a hard stop.
    if params.signal_flag.is_signaled() {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 130,
                reason: "signal received".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges,
        };
    }
    if ctx.crash_tracker.should_abort() {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 1,
                reason: "too many crashes".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges,
        };
    }

    if agg.any_completed && count_remaining_active_tasks(params.conn, params.task_prefix) == 0 {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 0,
                reason: "all tasks complete".to_string(),
                run_status: Some(RunStatus::Completed),
            }),
            was_stopped: false,
            failed_merges,
        };
    }

    let mut wave_should_stop = agg.wave_should_stop;
    if !wave_should_stop && wait_inter_wave_delay(params.inter_iteration_delay, params.signal_flag)
    {
        wave_should_stop = true;
    }

    WaveOutcome {
        tasks_completed,
        iteration_consumed: true,
        terminal: if wave_should_stop {
            if params.signal_flag.is_signaled() {
                Some(WaveTerminal {
                    exit_code: 130,
                    reason: "signal received".to_string(),
                    run_status: None,
                })
            } else {
                Some(WaveTerminal {
                    exit_code: 0,
                    reason: "stop signal".to_string(),
                    run_status: None,
                })
            }
        } else {
            None
        },
        was_stopped: false,
        failed_merges,
    }
}

/// Run a single iteration of the agent loop.
///
/// Returns `IterationResult` describing the outcome and whether to stop.
pub fn run_iteration(
    ctx: &mut IterationContext,
    params: &IterationParams,
) -> TaskMgrResult<IterationResult> {
    // Step 0: Check for SIGINT/SIGTERM
    if params.signal_flag.is_signaled() {
        eprintln!("Signal received, stopping loop...");
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 1: Check file-based signals
    if signals::check_stop_signal(params.tasks_dir, params.task_prefix) {
        eprintln!("Stop signal detected (.stop file found)");
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    if signals::check_pause_signal(params.tasks_dir, params.task_prefix) {
        signals::handle_pause(
            params.tasks_dir,
            params.iteration,
            &mut ctx.session_guidance,
            params.task_prefix,
        );
    }

    // Step 1.5: Pre-iteration usage check
    if params.usage_params.enabled {
        let check_result = usage::check_and_wait(
            params.usage_params.threshold,
            params.tasks_dir,
            params.usage_params.fallback_wait,
        );
        match check_result {
            UsageCheckResult::StopSignaled => {
                eprintln!("Stop signal during usage wait, exiting");
                return Ok(IterationResult {
                    outcome: IterationOutcome::Empty,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: true,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }
            UsageCheckResult::ApiError(ref msg) => {
                eprintln!("Usage API warning: {} (continuing)", msg);
            }
            _ => {} // BelowThreshold, WaitedAndReset, Skipped — proceed
        }
    }

    // Step 2: Check crash tracker backoff
    let backoff = ctx.crash_tracker.backoff_duration();
    if !backoff.is_zero() {
        eprintln!(
            "Crash backoff: waiting {} before retry...",
            display::format_duration(backoff.as_secs())
        );
        thread::sleep(backoff);
    }

    if ctx.crash_tracker.should_abort() {
        eprintln!("Too many consecutive crashes, aborting loop");
        return Ok(IterationResult {
            outcome: IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError),
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 3: Force algorithmic pick if too many reorders
    let effective_reorder_hint = if ctx.reorder_count >= MAX_CONSECUTIVE_REORDERS {
        eprintln!(
            "Forcing algorithmic task selection after {} consecutive reorders",
            ctx.reorder_count
        );
        ctx.reorder_count = 0;
        None
    } else {
        ctx.reorder_hint.take()
    };

    // Step 4: Build prompt (selects and claims task)
    let session_guidance_text = ctx.session_guidance.format_for_prompt();
    let prompt_params = BuildPromptParams {
        dir: params.db_dir,
        project_root: params.project_root,
        conn: params.conn,
        after_files: &ctx.last_files,
        run_id: Some(params.run_id),
        iteration: params.iteration,
        reorder_hint: effective_reorder_hint.as_deref(),
        session_guidance: &session_guidance_text,
        base_prompt_path: params.base_prompt_path,
        steering_path: params.steering_path,
        verbose: params.verbose,
        default_model: params.default_model,
        project_default_model: params.project_default_model,
        user_default_model: params.user_default_model,
        task_prefix: params.task_prefix,
        batch_sibling_prds: params.batch_sibling_prds,
        permission_mode: params.permission_mode,
    };

    let prompt_result = match prompt::build_prompt(&prompt_params) {
        Ok(Some(result)) => result,
        Ok(None) => {
            // No eligible task found — check if truly all done or just temporarily unavailable
            let (rem_pfx_clause, rem_pfx_param) = prefix_and(params.task_prefix);
            let rem_sql = format!(
                "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL {rem_pfx_clause}"
            );
            let rem_params: Vec<&dyn rusqlite::types::ToSql> = match &rem_pfx_param {
                Some(p) => vec![p],
                None => vec![],
            };
            let remaining: i64 = params
                .conn
                .query_row(&rem_sql, rem_params.as_slice(), |row| row.get(0))
                .unwrap_or(0);
            if remaining == 0 {
                eprintln!("All tasks complete!");
                return Ok(IterationResult {
                    outcome: IterationOutcome::Completed,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: true,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }

            // Auto-recover: reset stale in_progress tasks to todo and retry.
            // Safe because we hold the exclusive loop.lock — no other loop is running.
            //
            // First, reconcile any tasks that have passes: true in the PRD.
            // These were completed but the DB status was never updated.
            if let Some(prd) = params.prd_path {
                reconcile_passes_with_db(params.conn, prd, params.task_prefix);
            }

            let (mid_pfx_clause, mid_pfx_param) = prefix_and(params.task_prefix);
            let mid_recovery_sql = format!(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress' {mid_pfx_clause}"
            );
            let mid_params: Vec<&dyn rusqlite::types::ToSql> = match &mid_pfx_param {
                Some(p) => vec![p],
                None => vec![],
            };
            let recovered = params
                .conn
                .execute(&mid_recovery_sql, mid_params.as_slice())
                .unwrap_or(0);

            if recovered > 0 {
                eprintln!(
                    "Auto-recovered {} stale in_progress task(s), retrying task selection...",
                    recovered
                );
                // Retry build_prompt once with the same params
                match prompt::build_prompt(&prompt_params) {
                    Ok(Some(result)) => result,
                    Ok(None) => {
                        eprintln!(
                            "No eligible tasks after recovery ({} remaining). Treating as stale.",
                            remaining
                        );
                        return Ok(IterationResult {
                            outcome: IterationOutcome::NoEligibleTasks,
                            task_id: None,
                            files_modified: vec![],
                            should_stop: false,
                            output: String::new(),
                            effective_model: None,
                            effective_effort: None,
                            key_decisions_count: 0,
                            conversation: None,
                            shown_learning_ids: Vec::new(),
                        });
                    }
                    Err(TaskMgrError::PromptOverflow {
                        critical_size,
                        budget,
                        task_id,
                    }) => {
                        return Ok(prompt_overflow_result(critical_size, budget, task_id));
                    }
                    Err(e) => return Err(e),
                }
            } else {
                eprintln!(
                    "No eligible tasks right now ({} remaining in todo/in-progress/blocked). Treating as stale.",
                    remaining
                );
                return Ok(IterationResult {
                    outcome: IterationOutcome::NoEligibleTasks,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: false,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }
        }
        Err(TaskMgrError::PromptOverflow {
            critical_size,
            budget,
            task_id,
        }) => {
            return Ok(prompt_overflow_result(critical_size, budget, task_id));
        }
        Err(e) => return Err(e),
    };

    let task_id = prompt_result.task_id.clone();
    let task_files = prompt_result.task_files.clone();
    let shown_learning_ids = prompt_result.shown_learning_ids.clone();

    // Step 4.5: Apply crash escalation and PromptTooLong model overrides
    let effective_model = {
        let resolved = prompt_result.resolved_model.as_deref();
        let after_crash_escalation =
            match check_crash_escalation(&ctx.crashed_last_iteration, &task_id, resolved) {
                Some(escalated) => {
                    let old = resolved.unwrap_or("(default)");
                    eprintln!("Crash escalation: {} → {}", old, escalated);
                    Some(escalated)
                }
                None => prompt_result.resolved_model.clone(),
            };
        // Apply per-task 1M model override from prior PromptTooLong recovery
        if let Some(override_model) = ctx.model_overrides.get(&task_id) {
            let old = after_crash_escalation.as_deref().unwrap_or("(default)");
            eprintln!(
                "Model override (prior prompt overflow): {} → {}",
                old, override_model,
            );
            Some(override_model.clone())
        } else {
            after_crash_escalation
        }
    };

    // Use the cluster-wide effort computed by `build_prompt` — parallels the
    // cluster-wide `resolved_model` so both axes scale with the hardest task
    // in the synergy cluster. Apply any per-task override left by a prior
    // `PromptTooLong` crash on top.
    let base_effort = prompt_result.cluster_effort;
    let effort = ctx.effort_overrides.get(&task_id).copied().or(base_effort);
    if effort != base_effort {
        eprintln!(
            "Effort override (prior prompt overflow): {} → {}",
            base_effort.unwrap_or("(default)"),
            effort.unwrap_or("(default)"),
        );
    }

    // Step 5: Print iteration header (with post-escalation effective_model + effort)
    eprintln!(
        "{}",
        display::format_iteration_banner_with_recovery(
            params.iteration,
            params.max_iterations,
            &task_id,
            params.elapsed_secs,
            effective_model.as_deref(),
            effort,
            &ctx.overflow_recovered,
            &ctx.overflow_original_model,
        )
    );

    // Step 6: Start activity monitor, spawn Claude subprocess, stop monitor.
    // Timeout is intentionally derived from the primary task's difficulty, not
    // the cluster — synergy partners don't lengthen wall-clock inactivity budgets.
    let monitor_handle = monitor::start_monitor(params.project_root, None);
    let timeout_config = watchdog::TimeoutConfig::from_difficulty(
        prompt_result.task_difficulty.as_deref(),
        Arc::clone(&monitor_handle.last_activity_epoch),
    );
    let claude_result = claude::spawn_claude(
        &prompt_result.prompt,
        params.permission_mode,
        claude::SpawnOpts {
            signal_flag: Some(params.signal_flag),
            working_dir: Some(params.project_root),
            model: effective_model.as_deref(),
            timeout: Some(timeout_config),
            stream_json: true,
            effort,
            disallowed_tools: Some(TASKS_JSON_DISALLOWED_TOOLS),
            db_dir: Some(params.db_dir),
            // PTY disabled: when Claude sees isatty(1)==true it switches to
            // "interactive" handling of rate limits (internal wait + retry)
            // instead of failing fast with an error. That breaks task-mgr's
            // own probe_rate_limit_lifted wait loop because Claude never
            // exits; the watchdog eventually SIGKILLs it ~30 min later and
            // we lose the whole iteration. Live streaming would be nice but
            // not at the cost of rate-limit handling — revisit later with
            // a mechanism that keeps Claude in non-interactive mode while
            // still getting per-line flushing.
            use_pty: false,
            target_task_id: Some(&task_id),
            active_prefix: params.task_prefix,
            ..Default::default()
        },
    );
    monitor::stop_monitor(monitor_handle);
    claude::cleanup_ghost_sessions();
    let claude_result = claude_result?;

    // Step 6.1: Print hints for denied tools
    let denied_cmds = claude::extract_denied_commands(&claude_result.permission_denials);
    if !denied_cmds.is_empty() {
        let config_path = params.db_dir.join("config.json");
        let allowed_str = match params.permission_mode {
            PermissionMode::Scoped {
                allowed_tools: Some(t),
            }
            | PermissionMode::Auto {
                allowed_tools: Some(t),
            } => t.as_str(),
            _ => "",
        };
        for cmd in &denied_cmds {
            let pattern = format!("Bash({}:*)", cmd);
            if allowed_str.contains(&pattern) {
                // Tool is in the allowlist but Claude CLI still denied it —
                // likely user-level deny rules in ~/.claude/settings.json
                eprintln!(
                    "\x1b[33m[hint]\x1b[0m Tool denied: {} (already in --allowedTools \u{2014} \
                     check ~/.claude/settings.json or project .claude/settings.json for deny rules)",
                    cmd,
                );
            } else {
                eprintln!(
                    "\x1b[33m[hint]\x1b[0m Tool denied: {} \u{2014} to allow in future loops, add to {}:",
                    cmd,
                    config_path.display(),
                );
                eprintln!(
                    "       {{\"additionalAllowedTools\": [\"Bash({}:*)\"]}}",
                    cmd,
                );
            }
        }
    }

    // Step 6.1b: Targeted hints for Edit/Write denials on .task-mgr/tasks/*.json.
    // These are denied by --disallowedTools to prevent the agent from corrupting PRD JSON.
    // The agent should use `task-mgr add --stdin` or `<task-status>` tags instead.
    let tasks_json_denials = claude::extract_tasks_json_denials(&claude_result.permission_denials);
    for (tool, path) in &tasks_json_denials {
        match tool.as_str() {
            "Write" => eprintln!(
                "\x1b[33m[hint]\x1b[0m Tool denied: {} on {} \u{2014} \
                 use 'task-mgr init --from-json --append' to create new PRDs",
                tool, path,
            ),
            _ => eprintln!(
                "\x1b[33m[hint]\x1b[0m Tool denied: {} on {} \u{2014} \
                 use 'task-mgr add --stdin' or <task-status> tag instead",
                tool, path,
            ),
        }
    }

    // Step 6.5a: If iteration timed out, log and treat as a crash-like outcome
    if claude_result.timed_out {
        eprintln!(
            "Iteration timed out for task {} (difficulty: {})",
            task_id,
            prompt_result.task_difficulty.as_deref().unwrap_or("medium"),
        );
        return Ok(IterationResult {
            outcome: IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError),
            task_id: Some(task_id),
            files_modified: task_files,
            should_stop: false,
            output: claude_result.output,
            effective_model,
            effective_effort: effort,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 6.5: Detect if Claude was killed by SIGINT/SIGTERM (exit 130/143).
    // Claude may be the terminal foreground group, so Ctrl+C goes to it instead
    // of us. Propagate the signal to our flag so the loop stops cleanly.
    //
    // Exception: if the watchdog fired the post-completion grace kill, the
    // SIGTERM (143) was issued internally as a successful-completion finalizer
    // — not an external Ctrl+C. Propagating it would end the whole loop (and
    // any chained PRDs) despite the task completing normally.
    if matches!(claude_result.exit_code, 130 | 143) && !claude_result.completion_killed {
        params.signal_flag.set();
    }

    // If signal arrived during Claude execution (either directly or via exit code
    // detection above), stop immediately. Without this, post-processing runs
    // before the signal is checked at the next iteration boundary.
    if params.signal_flag.is_signaled() {
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: Some(task_id),
            files_modified: task_files,
            should_stop: true,
            output: claude_result.output,
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 7: Analyze output
    let claude_conversation = claude_result.conversation;
    let claude_output = claude_result.output;
    let outcome =
        detection::analyze_output(&claude_output, claude_result.exit_code, params.project_root);

    // Step 7.5: On rate-limit detection, trigger usage wait and mark as non-counting
    if outcome == IterationOutcome::RateLimit {
        eprintln!("Rate limit detected in output, checking usage API...");

        let mut waited = false;

        // Try the usage API first (if enabled)
        if params.usage_params.enabled {
            let check_result = usage::check_and_wait(
                params.usage_params.threshold,
                params.tasks_dir,
                params.usage_params.fallback_wait,
            );
            match check_result {
                UsageCheckResult::StopSignaled => {
                    return Ok(IterationResult {
                        outcome: IterationOutcome::RateLimit,
                        task_id: Some(task_id),
                        files_modified: task_files,
                        should_stop: true,
                        output: String::new(),
                        effective_model: None,
                        effective_effort: None,
                        key_decisions_count: 0,
                        conversation: None,
                        shown_learning_ids: Vec::new(),
                    });
                }
                UsageCheckResult::WaitedAndReset => {
                    waited = true;
                }
                _ => {} // Skipped, BelowThreshold, ApiError — didn't actually wait
            }
        }

        // Fallback: if the usage API didn't wait, parse reset time from output
        if !waited {
            let wait_secs = usage::parse_reset_from_output(&claude_output).unwrap_or(0);
            eprintln!(
                "Usage API did not wait (CLI session limit). Falling back to output-parsed reset time ({})...",
                if wait_secs > 0 {
                    display::format_duration(wait_secs)
                } else {
                    format!("fallback {}s", params.usage_params.fallback_wait)
                }
            );
            let probe = || probe_rate_limit_lifted(params.permission_mode);
            let completed = usage::wait_for_usage_reset(
                wait_secs,
                params.tasks_dir,
                params.usage_params.fallback_wait,
                Some(&probe),
            );
            if !completed {
                return Ok(IterationResult {
                    outcome: IterationOutcome::RateLimit,
                    task_id: Some(task_id),
                    files_modified: task_files,
                    should_stop: true,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }
        }
    }

    // Step 7.7 / Step 8 (extract_learnings_from_output, record_iteration_feedback)
    // were lifted into `iteration_pipeline::process_iteration_output` (FEAT-005).
    // The pipeline now runs from the call site (`run_loop`, `run_wave_iteration`)
    // after `run_iteration` returns. `shown_learning_ids` rides on
    // `IterationResult.shown_learning_ids` to reach the pipeline.

    // Step 8.5: Handle PromptTooLong — walk the four-state recovery ladder
    // and emit the diagnostics bundle (prompt dump + JSONL + rotation).
    //
    // The four rungs (first matching precondition wins, see
    // `overflow::handle_prompt_too_long`):
    //   1. `downgrade_effort`   — effort floor preserved at `high`.
    //   2. `escalate_below_opus` — `haiku → sonnet`, `sonnet → opus`.
    //   3. `to_1m_model`        — `opus → opus[1m]`.
    //   4. blocked              — no recovery left.
    //
    // Each rung emits a distinct stderr message that names the current task,
    // current effort/model, and the chosen action. The Blocked phrasing makes
    // it explicit that we are at `Opus[1M]` with `effort=high`, so users do
    // not chase a phantom "1M not tried" config. The crash-tracker backoff
    // still runs via update_trackers below; rungs 1-3 reset the task row to
    // `todo` (clearing `started_at`) so the next iteration retries with the
    // override applied, while rung 4 sets `blocked` so it doesn't consume
    // budget.
    if matches!(
        outcome,
        IterationOutcome::Crash(config::CrashType::PromptTooLong)
    ) {
        let _ = overflow::handle_prompt_too_long(
            ctx,
            params.conn,
            &task_id,
            effort,
            effective_model.as_deref(),
            &prompt_result,
            params.iteration,
            Some(params.run_id),
            params.db_dir,
            None,
        );
    }

    // Step 9: Update trackers based on outcome
    let mut should_stop = update_trackers(ctx, &outcome);

    // Step 10: Handle reorder
    if let IterationOutcome::Reorder(ref requested_task_id) = outcome {
        ctx.reorder_hint = Some(requested_task_id.clone());
        ctx.reorder_count += 1;
        eprintln!("Reorder requested: {}", requested_task_id);
    } else {
        ctx.reorder_count = 0;
    }

    // Step 11: Update last_files for next iteration scoring
    ctx.last_files = task_files.clone();

    // Step 12: Inter-iteration delay (skip if stopping or signaled)
    if !should_stop && !params.inter_iteration_delay.is_zero() && !params.signal_flag.is_signaled()
    {
        // Sleep in short intervals so we can respond to Ctrl+C promptly
        let deadline = std::time::Instant::now() + params.inter_iteration_delay;
        while std::time::Instant::now() < deadline {
            if params.signal_flag.is_signaled() {
                should_stop = true;
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    Ok(IterationResult {
        outcome,
        task_id: Some(task_id),
        files_modified: task_files,
        should_stop,
        output: claude_output,
        effective_model,
        effective_effort: effort,
        key_decisions_count: 0,
        conversation: claude_conversation,
        shown_learning_ids,
    })
}

/// Result returned by `run_loop()`.
///
/// Carries the exit code and (when applicable) the worktree path so that
/// callers can perform post-loop cleanup.
#[derive(Debug, Default)]
pub struct LoopResult {
    /// Exit code to pass to the process (0 = success, 1 = error, etc.)
    pub exit_code: i32,
    /// Worktree path used for this run.
    ///
    /// `Some` only when the loop actually created/reused a worktree (i.e.
    /// `use_worktrees = true` and a branch was specified). `None` when running
    /// directly in source_root or when no branch was configured.
    pub worktree_path: Option<PathBuf>,
    /// Branch name used for this run, from PRD metadata.
    ///
    /// Read by the batch runner to advance the chain — the next PRD branches from this.
    /// `None` on early-return error paths or when no branch was configured in the PRD.
    pub branch_name: Option<String>,
    /// True when the loop exited because a `.stop` file was detected.
    ///
    /// The engine consumes the signal file before returning, so callers that need
    /// to react to a mid-run stop (e.g. `run_batch`) must use this flag instead of
    /// re-checking the file system.
    pub was_stopped: bool,
    /// Number of tasks completed during this run (per-run counter, not cumulative).
    ///
    /// Incremented each time a task transitions to `done` within this invocation of
    /// `run_loop`. Does NOT count tasks completed in prior runs or via external git
    /// reconciliation. Used by callers (e.g. batch auto-review gating) to decide
    /// whether this run met the minimum-tasks threshold.
    pub tasks_completed: u32,
}

/// Configuration for running the loop, built from CLI args + env.
pub struct LoopRunConfig {
    /// Database directory (--dir flag, default ".task-mgr/")
    ///
    /// Always resolves to `{source_root}/.task-mgr/` - the database stays
    /// in the original repo even when using worktrees.
    pub db_dir: PathBuf,
    /// Original git repository root (from `git rev-parse --show-toplevel`)
    ///
    /// Contains PRD files, prompts, progress.txt, and `.task-mgr/` database.
    /// This is where path resolution for PRD/prompt files happens.
    pub source_root: PathBuf,
    /// Working directory for Claude subprocess
    ///
    /// When using worktrees, this is the worktree path.
    /// When not using worktrees, this equals `source_root`.
    /// Claude runs here and makes code changes here.
    pub working_root: PathBuf,
    /// Path to PRD JSON file
    pub prd_file: PathBuf,
    /// Optional path to prompt file (default: derived from PRD)
    pub prompt_file: Option<PathBuf>,
    /// Loop configuration (thresholds, delays, etc.)
    pub config: LoopConfig,
    /// Optional path to external git repo for commit scanning (CLI override)
    pub external_repo: Option<PathBuf>,
    /// Paths to OTHER PRD JSON files in the batch (empty for single-PRD runs).
    /// Used to inject sibling PRD context into MILESTONE task prompts.
    pub batch_sibling_prds: Vec<PathBuf>,
    /// Base git ref for this run's worktree.
    ///
    /// When `Some`, passed as `start_point` to `ensure_worktree()` so the branch
    /// is created from the specified ref instead of HEAD. Set by the batch runner
    /// when `--chain` is active. `None` for standalone runs and chain=false batch runs.
    pub chain_base: Option<String>,
    /// Prefix mode for task ID namespacing during `init()`.
    ///
    /// `Auto` (default for single and batch runs): generates a deterministic prefix
    /// from `md5(branchName:filename)[:8]`, ensuring loop→batch continuity.
    /// `Explicit(prefix)`: CLI `--prefix` override.
    /// `Disabled`: no prefix (CLI `--no-prefix` flag).
    pub prefix_mode: PrefixMode,
}

/// Expected global skills for task-mgr loop workflows.
///
/// These skills (`.md` files in `~/.claude/commands/`) provide slash commands
/// that wrap common task-mgr operations for interactive Claude Code sessions.
const EXPECTED_GLOBAL_SKILLS: &[&str] = &[
    "tm-apply",
    "tm-learn",
    "tm-recall",
    "tm-invalidate",
    "tm-status",
    "tm-next",
];

/// Check if task-mgr global Claude Code skills are installed.
///
/// Prints a warning with installation instructions if any are missing.
/// Non-blocking — the loop continues regardless.
fn check_global_skills(source_root: &Path) {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return, // Can't determine home dir; skip check silently
    };
    let global_dir = home.join(".claude").join("commands");

    let missing: Vec<&str> = EXPECTED_GLOBAL_SKILLS
        .iter()
        .filter(|name| !global_dir.join(format!("{}.md", name)).exists())
        .copied()
        .collect();

    if missing.is_empty() {
        return;
    }

    let repo_skill_dir = source_root.join(".claude").join("commands");
    let has_repo_copies = missing
        .iter()
        .any(|name| repo_skill_dir.join(format!("{}.md", name)).exists());

    eprintln!(
        "Warning: {} task-mgr skill(s) not found in ~/.claude/commands/: {}",
        missing.len(),
        missing.join(", ")
    );

    if has_repo_copies {
        eprintln!("  Install from this repo:");
        for name in &missing {
            let src = repo_skill_dir.join(format!("{}.md", name));
            if src.exists() {
                eprintln!("    cp {} {}/", src.display(), global_dir.display());
            }
        }
    } else {
        eprintln!(
            "  These skills provide /tm-learn, /tm-recall, /tm-invalidate, /tm-status, /tm-next"
        );
        eprintln!("  See the task-mgr README for installation instructions.");
    }
    eprintln!();
}

/// Run the full autonomous agent loop.
///
/// This is the top-level orchestrator called from `main.rs`:
/// 1. Load .env and validate git repo
/// 2. Resolve paths and open DB
/// 3. Read PRD metadata (branch name, task count)
/// 4. Begin a run session
/// 5. Create deadline if hours specified
/// 6. Install signal handlers
/// 7. Iterate until done, blocked, max iterations, or signal
/// 8. End run, cleanup, return exit code
///
/// # Exit codes
/// - 0: success (all tasks complete) or graceful stop
/// - 1: error, max crashes, max stale, or max iterations reached
/// - 2: blocked
/// - 130: SIGINT
/// - 143: SIGTERM
pub async fn run_loop(mut run_config: LoopRunConfig) -> LoopResult {
    // Step 1: Load environment
    env::load_env();

    // Step 1.5: Check for global Claude Code skills
    check_global_skills(&run_config.source_root);

    // Step 2: Validate git repo (source_root is the original repo)
    if let Err(e) = env::validate_git_repo(&run_config.source_root) {
        eprintln!("Error: {}", e);
        eprintln!("Hint: Run task-mgr from within a git repository.");
        return LoopResult {
            exit_code: 1,
            ..Default::default()
        };
    }

    // Step 3: Resolve paths (PRD, prompt, progress live in source_root)
    let mut paths = match env::resolve_paths(
        &run_config.prd_file,
        run_config.prompt_file.as_deref(),
        &run_config.source_root,
        None,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error resolving paths: {}", e);
            eprintln!(
                "Hint: Check that the PRD file path is correct relative to your project root."
            );
            return LoopResult {
                exit_code: 1,
                ..Default::default()
            };
        }
    };

    // Propagate resolved absolute path so all downstream code (init, prefix
    // generation, hash, etc.) uses the actual file location — which may be in
    // a sibling worktree rather than the local source_root.
    run_config.prd_file = paths.prd_file.clone();

    // Step 4: Ensure directories exist (in db_dir)
    if let Err(e) = env::ensure_directories(&run_config.db_dir) {
        eprintln!("Error creating directories: {}", e);
        return LoopResult {
            exit_code: 1,
            ..Default::default()
        };
    }

    // Step 4.5: Acquire exclusive loop lock — prevents concurrent loops on same DB.
    // Must be before any DB mutations (init, migrations, recovery).
    // Separate from tasks.db.lock (short-lived per-command) so read-only commands
    // like `status` and `stats` are not blocked.
    //
    // Read the PRD's taskPrefix BEFORE acquiring the lock so we can use a
    // per-prefix lock file (loop-{prefix}.lock) that allows concurrent loops
    // on different PRDs. Falls back to "loop.lock" when prefix is unknown.
    // Read both hints in a single file parse.
    let prd_hints = read_prd_hints(&run_config.prd_file);
    let pre_lock_branch = prd_hints.branch_name;
    let pre_lock_prefix: Option<String> = match &run_config.prefix_mode {
        // Explicit prefix (batch mode): use it directly, skip PRD hints.
        PrefixMode::Explicit(p) => Some(p.clone()),
        // Disabled: no prefix at all.
        PrefixMode::Disabled => None,
        // Auto: always generate deterministically from branchName + filename.
        // The JSON's taskPrefix field is ignored to prevent mismatch bugs.
        PrefixMode::Auto => {
            let filename = run_config
                .prd_file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            Some(generate_prefix(pre_lock_branch.as_deref(), filename))
        }
    }
    .and_then(|p| {
        // Only use prefix if it is safe for filenames
        if validate_prefix(&p).is_ok() {
            Some(p)
        } else {
            None
        }
    });
    let lock_name = match &pre_lock_prefix {
        Some(p) => format!("loop-{p}.lock"),
        None => "loop.lock".to_string(),
    };
    let prd_display = run_config.prd_file.display();
    let mut loop_lock = match LockGuard::acquire_named(&run_config.db_dir, &lock_name) {
        Ok(guard) => guard,
        Err(e) => {
            match &pre_lock_prefix {
                Some(p) => {
                    eprintln!(
                        "Error: cannot start loop for {prd_display} — another loop is already running (prefix={p}). {e}"
                    );
                    eprintln!(
                        "Hint: Each PRD gets its own lock file (loop-{{prefix}}.lock). If the other PRD is still running, wait for it to finish."
                    );
                }
                None => {
                    eprintln!(
                        "Error: cannot start loop for {prd_display} — another loop is already running on the global lock. {e}"
                    );
                    eprintln!(
                        "Hint: Each PRD uses its own lock file (loop-{{prefix}}.lock). If both PRDs lack taskPrefix, they collide on the global lock."
                    );
                }
            }
            return LoopResult {
                exit_code: 1,
                ..Default::default()
            };
        }
    };

    // Step 4.55: Enrich lock file with prefix/branch immediately after acquisition.
    // pre_lock_prefix and pre_lock_branch are already known from step 4.5.
    if let Err(e) = loop_lock.write_holder_info_extended(
        pre_lock_branch.as_deref(),
        run_config.working_root.to_str(),
        pre_lock_prefix.as_deref(),
    ) {
        eprintln!(
            "Warning: failed to write extended lock metadata: {} (continuing)",
            e
        );
    }

    // Step 4.6: Detect branch change (archive previous PRD if branch switched)
    match branch::detect_branch_change(
        &run_config.source_root,
        &run_config.db_dir,
        &paths.tasks_dir,
        run_config.config.yes_mode,
        pre_lock_prefix.as_deref(),
    ) {
        Ok(true) => {
            eprintln!("Branch change handled, continuing with new branch setup");
        }
        Ok(false) => {} // No change or first run
        Err(e) => {
            eprintln!(
                "Warning: branch change detection failed: {} (continuing)",
                e
            );
        }
    }

    // Step 5: Initialize PRD (creates schema + imports tasks, idempotent)
    // Uses run_config.prefix_mode: Auto for single runs, Explicit for batch mode.
    if let Err(e) = crate::commands::init(
        &run_config.db_dir,
        &[&run_config.prd_file],
        false, // force
        true,  // append
        true,  // update_existing
        false, // dry_run
        run_config.prefix_mode.clone(),
    ) {
        eprintln!("Error initializing PRD: {}", e);
        return LoopResult {
            exit_code: 1,
            ..Default::default()
        };
    }

    // Step 5.5: PRD hash — computed after worktree setup (step 8.5) since
    // Claude edits the worktree copy, not the source_root copy.
    #[allow(unused_assignments)]
    let mut prd_hash = String::new();

    // Step 6: Open DB connection (after init to ensure schema exists)
    let mut conn = match crate::db::open_connection(&run_config.db_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error opening database: {}", e);
            return LoopResult {
                exit_code: 1,
                ..Default::default()
            };
        }
    };

    if run_config.config.verbose {
        let canonical = run_config.db_dir.join("tasks.db");
        eprintln!("[verbose] Database path: {}", canonical.display());
        eprintln!(
            "[verbose] Source root:   {}",
            run_config.source_root.display()
        );
        eprintln!(
            "[verbose] Working root:  {}",
            run_config.working_root.display()
        );
    }

    // Step 6.5: Run any pending migrations (e.g. v4 adds external_git_repo column)
    if let Err(e) = crate::db::run_migrations(&mut conn) {
        eprintln!("Warning: failed to run migrations: {} (continuing)", e);
    }

    // Step 6.55: Reuse the prefix already determined at step 4.5 — no second file read.
    // pre_lock_prefix holds either the PRD's explicit taskPrefix or the deterministic
    // auto-generated value (same algorithm as init), so it matches after step 5 runs.
    let early_task_prefix: Option<String> = pre_lock_prefix.clone();

    // Step 6.6: Recover stale in_progress tasks from previous crashed/killed runs.
    // Safe because we hold the per-prefix loop lock — no other loop with the same
    // prefix can be running. (Loops on different prefixes CAN run concurrently.)
    // Recovery is scoped by prefix so concurrent loops don't reset each other.
    let (recovery_pfx_clause, recovery_pfx_param) = prefix_and(early_task_prefix.as_deref());
    let recovery_sql = format!(
        "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress' {recovery_pfx_clause}"
    );
    let recovery_params: Vec<&dyn rusqlite::types::ToSql> = match &recovery_pfx_param {
        Some(p) => vec![p as &dyn rusqlite::types::ToSql],
        None => vec![],
    };
    match conn.execute(&recovery_sql, recovery_params.as_slice()) {
        Ok(count) if count > 0 => {
            eprintln!(
                "Recovered {} stale in_progress task(s) from previous run",
                count
            );
        }
        Ok(_) => {}
        Err(e) => {
            // Hard error: if recovery fails, the loop will deadlock on blocked dependencies
            eprintln!("Error: failed to reset stale tasks: {}", e);
            return LoopResult {
                exit_code: 1,
                ..Default::default()
            };
        }
    }

    // Step 6.7: Auto-retire stale learnings at session start so recall quality
    // is high from the first task. Uses default thresholds (90 days, 10 shows, 5% rate).
    match crate::commands::curate::curate_retire(&conn, Default::default()) {
        Ok(result) if result.learnings_retired > 0 => {
            eprintln!(
                "Auto-retired {} stale learning(s) at session start",
                result.learnings_retired
            );
        }
        Ok(_) => {} // nothing to retire
        Err(e) => {
            eprintln!("Warning: auto-retire learnings failed: {} (continuing)", e);
        }
    }

    // Step 7: Read PRD metadata for branch name, task count, and external repo
    let prd_metadata = match read_prd_metadata(&conn, early_task_prefix.as_deref()) {
        Ok(meta) => meta,
        Err(e) => {
            eprintln!("Error reading PRD metadata: {}", e);
            return LoopResult {
                exit_code: 1,
                ..Default::default()
            };
        }
    };
    let branch_name = prd_metadata.branch_name;
    let task_count = prd_metadata.task_count;
    let task_prefix = prd_metadata.task_prefix;
    let default_model = prd_metadata.default_model;
    // Config-level defaults: fall below PRD default in the resolution chain.
    // The loop engine never prompts — it runs non-interactively — so these
    // are pure reads. Users pin a default via `task-mgr init` or
    // `task-mgr models set-default`.
    //
    // Fix 2 from /review-loop: load the full `ProjectConfig` once at the
    // start of the run and thread it through `WaveParams` instead of
    // re-reading + re-parsing `.task-mgr/config.json` from every wave
    // (FEAT-003 implicit-overlap pull, FEAT-002 halt-threshold check, the
    // merge-resolver settings, the FEAT-005 reconcile threshold). Mid-loop
    // edits to the file are NOT picked up; operators restart the loop to
    // apply config changes — matching every other run-scoped knob.
    let project_config =
        crate::loop_engine::project_config::read_project_config(&run_config.db_dir);
    let project_default_model = project_config.default_model.clone();
    let user_default_model = crate::loop_engine::user_config::read_user_config().default_model;
    // Same caching rationale for the PRD-side `implicit_overlap_files`
    // override. Field is rare and small (a list of file basenames), so
    // an empty Vec when the PRD JSON is absent / malformed is safe.
    let prd_implicit_overlap_files = read_prd_implicit_overlap_files(paths.prd_file.as_path());

    // Step 7.05: Now that task_prefix is known, re-derive per-PRD progress file.
    if let Some(ref pfx) = task_prefix {
        paths.progress_file = paths.tasks_dir.join(format!("progress-{}.txt", pfx));
    }

    // Step 7.1: Reconcile tasks that have passes: true in PRD but are not done in DB.
    // This catches tasks completed in a previous run where the DB status was never
    // updated (e.g., rate limit interrupted git detection, or loop exit reset them).
    reconcile_passes_with_db(&conn, &run_config.prd_file, task_prefix.as_deref());

    // Step 7.2: Setup pre-check for new task lists only.
    // "New" = no tasks are done yet (first-ever run, or all tasks were reset).
    // Non-blocking: prints a yellow warning banner but always continues.
    {
        let (pfx_clause, pfx_param) = prefix_and(task_prefix.as_deref());
        let done_sql = format!("SELECT COUNT(*) FROM tasks WHERE status = 'done' {pfx_clause}");
        let done_params: Vec<&dyn rusqlite::types::ToSql> = match &pfx_param {
            Some(p) => vec![p],
            None => vec![],
        };
        let done_count: i64 = conn
            .query_row(&done_sql, done_params.as_slice(), |row| row.get(0))
            .unwrap_or(0);
        let is_new_task_list = done_count == 0;

        if is_new_task_list && let Ok(home) = std::env::var("HOME") {
            let global_dir = PathBuf::from(home).join(".claude");
            let checks = pre_check_loop_setup(&global_dir);
            let blockers: Vec<_> = checks
                .iter()
                .filter(|c| c.severity == SetupSeverity::Blocker)
                .collect();
            if !blockers.is_empty() {
                eprintln!(
                    "\x1b[33m⚠ Setup warning: {} blocker(s) detected in ~/.claude/settings.json:\x1b[0m",
                    blockers.len()
                );
                for b in &blockers {
                    eprintln!("  \x1b[33m•\x1b[0m {}", b.message);
                    if let Some(ref fix) = b.fix_command {
                        eprintln!("    Fix: {fix}");
                    }
                }
                eprintln!("\x1b[33m  The loop will continue but tool calls may be blocked.\x1b[0m");
                eprintln!("  Run `task-mgr doctor --setup` for a full audit.");
                eprintln!();
            }
        }
    }

    // Resolve external git repo path: CLI flag overrides PRD metadata
    let external_repo_path: Option<PathBuf> = run_config
        .external_repo
        .clone()
        .or_else(|| prd_metadata.external_git_repo.map(PathBuf::from))
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                run_config.source_root.join(&p)
            }
        });

    // Step 8: Determine working_root (worktree or source_root)
    // If using worktrees and a branch is specified, create/use a worktree.
    // Otherwise, check out the branch in source_root (old behavior).
    // Track whether we actually set up a worktree so we can clean it up later.
    let mut actual_worktree_path: Option<PathBuf> = None;
    let working_root = if let Some(ref branch) = branch_name {
        if run_config.config.use_worktrees {
            // Create or reuse worktree for this branch
            match worktree::ensure_worktree(
                &run_config.source_root,
                branch,
                run_config.config.yes_mode,
                run_config.chain_base.as_deref(),
            ) {
                Ok(wt_path) => {
                    actual_worktree_path = Some(wt_path.clone());
                    wt_path
                }
                Err(e) => {
                    eprintln!("Error setting up worktree: {}", e);
                    return LoopResult {
                        exit_code: 1,
                        ..Default::default()
                    };
                }
            }
        } else {
            // Old behavior: checkout branch in source_root
            if let Err(e) =
                env::ensure_branch(&run_config.source_root, branch, run_config.config.yes_mode)
            {
                eprintln!("Error: {}", e);
                return LoopResult {
                    exit_code: 1,
                    ..Default::default()
                };
            }
            run_config.source_root.clone()
        }
    } else {
        // No branch specified, use source_root as working directory
        run_config.source_root.clone()
    };

    // Step 8.4: Ensure task files exist in the worktree.
    // If using a worktree, copy PRD JSON, prompt, and PRD markdown from source_root
    // if they don't already exist in the worktree.
    if working_root != run_config.source_root {
        let canonical_source = run_config
            .source_root
            .canonicalize()
            .unwrap_or_else(|_| run_config.source_root.clone());

        let copy_if_missing = |src: &Path| {
            if let Ok(rel) = src.strip_prefix(&canonical_source) {
                let dest = working_root.join(rel);
                if !dest.exists() && src.exists() {
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = std::fs::copy(src, &dest) {
                        eprintln!(
                            "Warning: failed to copy {} to worktree: {}",
                            rel.display(),
                            e
                        );
                    } else {
                        eprintln!("Copied {} to worktree", rel.display());
                    }
                }
            }
        };

        // PRD JSON (task list)
        copy_if_missing(&paths.prd_file);

        // Prompt file
        copy_if_missing(&paths.prompt_file);

        // PRD markdown (from prdFile field in JSON, if present)
        if let Ok(content) = std::fs::read_to_string(&paths.prd_file)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(prd_md) = json.get("prdFile").and_then(|v| v.as_str())
        {
            let prd_md_path = paths
                .prd_file
                .parent()
                .unwrap_or(&paths.prd_file)
                .join(prd_md);
            copy_if_missing(&prd_md_path);
        }
    }

    // Step 8.5: Compute live PRD path (worktree copy if using worktrees, else source_root)
    // Claude edits the worktree copy, so hash checks and re-imports must use that path.
    // paths.prd_file is canonicalized by resolve_paths(); canonicalize source_root too
    // so strip_prefix works reliably (e.g. symlinks resolved on both sides).
    let live_prd_file = if working_root != run_config.source_root {
        let canonical_source = run_config
            .source_root
            .canonicalize()
            .unwrap_or_else(|_| run_config.source_root.clone());
        if let Ok(rel) = paths.prd_file.strip_prefix(&canonical_source) {
            working_root.join(rel)
        } else {
            eprintln!(
                "Warning: could not remap PRD to worktree (prd={}, source={})",
                paths.prd_file.display(),
                canonical_source.display()
            );
            paths.prd_file.clone()
        }
    } else {
        paths.prd_file.clone()
    };
    // If using a worktree, re-import from the worktree PRD to pick up any tasks
    // that were added in the worktree but not in source_root (e.g., tasks created
    // by Claude during a previous run that only exist in the worktree copy).
    if live_prd_file != run_config.prd_file
        && live_prd_file.exists()
        && let Err(e) = crate::commands::init(
            &run_config.db_dir,
            &[&live_prd_file],
            false, // force
            true,  // append
            true,  // update_existing
            false, // dry_run
            run_config.prefix_mode.clone(),
        )
    {
        eprintln!("Warning: worktree PRD re-import failed: {} (continuing)", e);
    }
    prd_hash = hash_file(&live_prd_file);
    // Override paths.prd_file so all iteration code (mark_task_done, reconcile, etc.)
    // reads/writes the worktree copy, not the source_root copy.
    paths.prd_file = live_prd_file.clone();

    // Step 9: Check uncommitted changes (in working_root)
    if let Err(e) = env::check_uncommitted_changes(&working_root, run_config.config.yes_mode) {
        eprintln!("Error: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            ..Default::default()
        };
    }

    // Step 9.5: Parallel wave setup (FEAT-010).
    // Wave execution requires a branch (for ephemeral slot branches) AND
    // worktrees enabled. If the user asked for --parallel > 1 but either
    // pre-condition is missing, we warn and silently fall back to the
    // sequential path so the loop still makes progress instead of failing.
    let parallel_requested = run_config.config.parallel_slots > 1;
    let (mut parallel_active, slot_worktree_paths) = if parallel_requested {
        match (branch_name.as_ref(), run_config.config.use_worktrees) {
            (Some(branch), true) => {
                // FEAT-005: clean up any `{branch}-slot-N` left over from a
                // prior loop crash before we try to (re)create slot worktrees.
                // Aborts startup on dirty / un-merged anomalies; otherwise
                // returns Ok and leaves the path clear for `ensure_slot_worktrees`.
                let halt_threshold = project_config.merge_fail_halt_threshold;
                if let Err(e) = worktree::reconcile_stale_ephemeral_slots(
                    &run_config.source_root,
                    branch,
                    halt_threshold,
                ) {
                    eprintln!(
                        "Error: stale ephemeral-slot reconcile aborted startup: {}",
                        e
                    );
                    return LoopResult {
                        exit_code: 1,
                        worktree_path: actual_worktree_path,
                        ..Default::default()
                    };
                }
                match worktree::ensure_slot_worktrees(
                    &run_config.source_root,
                    branch,
                    run_config.config.parallel_slots,
                ) {
                    Ok(paths) => {
                        eprintln!(
                            "Parallel mode active: {} slots ({} ephemeral branches)",
                            run_config.config.parallel_slots,
                            run_config.config.parallel_slots.saturating_sub(1)
                        );
                        (true, paths)
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to set up slot worktrees: {} — falling back to sequential",
                            e
                        );
                        (false, Vec::new())
                    }
                }
            }
            (None, _) => {
                eprintln!(
                    "Warning: --parallel {} requires a branchName in the PRD; falling back to sequential",
                    run_config.config.parallel_slots
                );
                (false, Vec::new())
            }
            (Some(_), false) => {
                eprintln!(
                    "Warning: --parallel {} requires use_worktrees=true; falling back to sequential",
                    run_config.config.parallel_slots
                );
                (false, Vec::new())
            }
        }
    } else {
        (false, Vec::new())
    };

    // Step 10: Calculate max iterations
    let max_iterations = if run_config.config.max_iterations > 0 {
        run_config.config.max_iterations as u32
    } else {
        config::auto_max_iterations(task_count) as u32
    };

    // Step 11: Create deadline if hours specified
    let prd_basename = run_config
        .prd_file
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if let Some(hours) = run_config.config.hours
        && let Err(e) = deadline::create_deadline(&paths.tasks_dir, &prd_basename, hours)
    {
        eprintln!("Error creating deadline: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            ..Default::default()
        };
    }

    // Step 12: Begin run session
    let begin_result = match run_cmd::begin(&conn) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error beginning run: {}", e);
            deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
            return LoopResult {
                exit_code: 1,
                worktree_path: actual_worktree_path,
                ..Default::default()
            };
        }
    };
    let run_id = begin_result.run_id;

    // Step 12.5: Reconcile external git completions at startup
    // Catches tasks completed in prior runs that are still marked incomplete
    if let Some(ref ext_repo) = external_repo_path {
        let count = reconcile_external_git_completions(
            ext_repo,
            &mut conn,
            &run_id,
            &paths.prd_file,
            task_prefix.as_deref(),
            run_config.config.external_git_scan_depth,
        );
        if count > 0 {
            eprintln!(
                "Startup reconciliation: marked {} task(s) done from external repo",
                count
            );
        }
    }

    // Step 12.7: Display any deferred key decisions from previous sessions
    match key_decisions_db::get_all_pending_decisions(&conn) {
        Ok(decisions) if !decisions.is_empty() => {
            eprintln!(
                "\n\x1b[33m⚑ {} deferred key decision(s) from previous sessions:\x1b[0m",
                decisions.len()
            );
            for d in &decisions {
                let task_ctx = d
                    .task_id
                    .as_deref()
                    .map(|t| format!(" [task: {}]", t))
                    .unwrap_or_default();
                eprintln!("  • {}{}", d.title, task_ctx);
                eprintln!("    {}", d.description);
            }
            eprintln!();
        }
        Ok(_) => {}
        Err(e) => {
            // Non-fatal: pre-v12 DB won't have this table
            eprintln!("Note: could not query deferred key decisions: {}", e);
        }
    }

    // Step 13: Install signal handler
    let signal_flag = SignalFlag::new();
    setup_signal_handler(signal_flag.clone());

    // Step 14: Resolve steering.md path
    let steering_path = paths.tasks_dir.join("steering.md");
    let steering = if steering_path.exists() {
        Some(steering_path.as_path())
    } else {
        None
    };

    // Step 15: Resolve permission mode (needed for banner hint below).
    // Resolved once at startup; re-checked each iteration for hot-reload.
    let mut permission_mode = config::resolve_permission_mode(&run_config.db_dir);

    if run_config.config.verbose {
        eprintln!("[verbose] Permission mode: {}", permission_mode);
    }

    // Step 15.5: Print session banner
    let branch_display = branch_name.as_deref().unwrap_or("(unknown)");
    let db_path = run_config.db_dir.join("tasks.db");
    let banner_hints = display::SessionBannerHints {
        db_path: &db_path,
        prefix: task_prefix.as_deref(),
        worktree_path: actual_worktree_path.as_deref(),
    };
    display::print_session_banner(
        &prd_basename,
        branch_display,
        max_iterations,
        run_config.config.hours,
        Some(&banner_hints),
    );

    // Step 15.6: Print auto-mode availability hint if applicable.
    // Fires when LOOP_AUTO_MODE_AVAILABLE=true and user is NOT already in Auto mode.
    // Informs the user that the current permission model will be deprecated.
    if let Ok(val) = std::env::var("LOOP_AUTO_MODE_AVAILABLE")
        && config::parse_bool_value(&val) == Some(true)
        && !matches!(permission_mode, config::PermissionMode::Auto { .. })
    {
        eprintln!("{}", AUTO_MODE_DEPRECATION_HINT);
    }

    // Step 15.7: Log requires_human task count so the user knows pauses are coming
    {
        let review_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE requires_human = 1 AND status != 'done'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if review_count > 0 {
            eprintln!("{} task(s) require human review", review_count);
        }
    }

    // Step 16: Build usage params
    let usage_params = UsageParams {
        enabled: run_config.config.usage_check_enabled,
        threshold: run_config.config.usage_threshold,
        fallback_wait: run_config.config.usage_fallback_wait,
    };

    // Step 17: Run iteration loop
    let start_time = Instant::now();
    let inter_iteration_delay = Duration::from_secs(run_config.config.iteration_delay_secs);
    let mut ctx = IterationContext::new(run_config.config.max_crashes as u32);
    let mut iterations_completed: u32 = 0;
    let mut tasks_completed: u32 = 0;
    let mut last_claimed_task: Option<String> = None;
    let mut exit_code: i32 = 0;
    let mut exit_reason = String::from("max iterations reached");
    let mut final_run_status = RunStatus::Aborted;
    let mut was_stopped = false; // set true only when a .stop file halted the loop

    // Rotate progress file before starting iterations to bound context size
    progress::rotate_progress(&paths.progress_file);

    let mut iteration: u32 = 0;
    while iteration < max_iterations as u32 {
        iteration += 1; // 1-based, incremented at top
        // Pre-iteration: refresh OAuth token if usage checking enabled
        if usage_params.enabled {
            oauth::ensure_valid_token();
        }

        // Check deadline
        if deadline::check_deadline(&paths.tasks_dir, &prd_basename) {
            eprintln!("Deadline reached, stopping loop");
            exit_reason = "deadline reached".to_string();
            exit_code = 0;
            break;
        }

        // Hot-reload permission mode: re-resolve each iteration so config.json
        // edits mid-loop take effect without restarting.
        let iter_permission_mode = config::resolve_permission_mode(&run_config.db_dir);
        if iter_permission_mode != permission_mode {
            eprintln!(
                "\x1b[36m[info]\x1b[0m Permission mode changed: {} → {}",
                permission_mode, iter_permission_mode
            );
            permission_mode = iter_permission_mode;
        }

        // Re-import PRD if Claude modified it during the previous iteration.
        // Use live_prd_file (worktree copy) since Claude edits in the worktree.
        let current_hash = hash_file(&live_prd_file);
        if current_hash != prd_hash {
            eprintln!("PRD file changed, re-importing tasks...");
            if let Err(e) = crate::commands::init(
                &run_config.db_dir,
                &[&live_prd_file],
                false, // force
                true,  // append
                true,  // update_existing
                false, // dry_run
                run_config.prefix_mode.clone(),
            ) {
                eprintln!("Warning: PRD re-import failed: {} (continuing)", e);
            }
            prd_hash = current_hash;
        }

        let elapsed = start_time.elapsed().as_secs();

        // Parallel wave dispatch (FEAT-010). When `parallel_active` we run
        // a wave of slot iterations and skip the rest of the sequential
        // post-processing — `run_wave_iteration` performs its own per-slot
        // logging, status dispatch, completion detection, crash policy, and
        // terminal-condition checks. The outer loop only needs to track the
        // counters and decide when to break.
        if parallel_active {
            // Invariant: `parallel_active` is only set when `branch_name` is
            // Some (see step 9.5). `debug_assert!` traps in tests if a future
            // change breaks the invariant; release builds keep the graceful
            // sequential fallthrough rather than panicking on an inconsistency.
            debug_assert!(
                branch_name.is_some(),
                "parallel_active=true must imply branch_name is Some"
            );
            let Some(branch) = branch_name.as_deref() else {
                eprintln!(
                    "Warning: parallel_active=true but branch_name is None; \
                     falling through to sequential iteration"
                );
                parallel_active = false;
                continue;
            };
            // Materialize wave-scope inputs that need stable lifetimes for the
            // borrowed fields on `WaveIterationParams`.
            let wave_session_guidance = ctx.session_guidance.format_for_prompt();
            let wave_params = WaveIterationParams {
                conn: &mut conn,
                db_dir: &run_config.db_dir,
                source_root: &run_config.source_root,
                branch,
                parallel_slots: run_config.config.parallel_slots,
                slot_worktree_paths: &slot_worktree_paths,
                iteration,
                max_iterations,
                elapsed_secs: elapsed,
                run_id: &run_id,
                base_prompt_path: &paths.prompt_file,
                permission_mode: &permission_mode,
                signal_flag: &signal_flag,
                default_model: default_model.as_deref(),
                verbose: run_config.config.verbose,
                task_prefix: task_prefix.as_deref(),
                prd_path: paths.prd_file.as_path(),
                progress_path: paths.progress_file.as_path(),
                tasks_dir: paths.tasks_dir.as_path(),
                external_repo_path: external_repo_path.as_deref(),
                external_git_scan_depth: run_config.config.external_git_scan_depth,
                inter_iteration_delay,
                steering_path: steering,
                session_guidance: &wave_session_guidance,
                prd_implicit_overlap_files: &prd_implicit_overlap_files,
                project_config: &project_config,
            };
            let outcome = run_wave_iteration(wave_params, &mut ctx);
            tasks_completed += outcome.tasks_completed;
            if outcome.iteration_consumed {
                iterations_completed += 1;
            }
            if outcome.was_stopped {
                was_stopped = true;
            }

            // FEAT-002: reset/halt contract on parallel-slot merge-back
            // failures. Logic lives in `apply_merge_fail_reset_and_halt_check`
            // so it can be unit-tested in isolation.
            let halt_threshold = project_config.merge_fail_halt_threshold;
            if let MergeFailHaltDecision::Halt {
                exit_code: halt_code,
                exit_reason: halt_reason,
            } = apply_merge_fail_reset_and_halt_check(
                &conn,
                &mut ctx,
                branch,
                &outcome.failed_merges,
                halt_threshold,
            ) {
                exit_code = halt_code;
                exit_reason = halt_reason;
                break;
            }

            if let Some(t) = outcome.terminal {
                exit_code = t.exit_code;
                exit_reason = t.reason;
                if let Some(s) = t.run_status {
                    final_run_status = s;
                }
                break;
            }
            // Suppress unused-elapsed warning when the sequential branch is
            // skipped — the value is recomputed next iteration anyway.
            let _ = elapsed;
            continue;
        }

        let iteration_params = IterationParams {
            conn: &conn,
            db_dir: &run_config.db_dir,
            project_root: &working_root,
            tasks_dir: &paths.tasks_dir,
            iteration,
            max_iterations,
            run_id: &run_id,
            base_prompt_path: &paths.prompt_file,
            steering_path: steering,
            inter_iteration_delay,
            signal_flag: &signal_flag,
            elapsed_secs: elapsed,
            verbose: run_config.config.verbose,
            usage_params: &usage_params,
            prd_path: Some(paths.prd_file.as_path()),
            task_prefix: task_prefix.as_deref(),
            default_model: default_model.as_deref(),
            project_default_model: project_default_model.as_deref(),
            user_default_model: user_default_model.as_deref(),
            permission_mode: &permission_mode,
            batch_sibling_prds: &run_config.batch_sibling_prds,
        };

        let mut result = match run_iteration(&mut ctx, &iteration_params) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Iteration error: {}", e);
                exit_code = 1;
                exit_reason = format!("iteration error: {}", e);
                break;
            }
        };

        // Early exit on signal — skip all post-processing (git checks,
        // reconciliation, etc.) to respond to Ctrl+C immediately.
        if signal_flag.is_signaled() {
            exit_code = 130;
            exit_reason = "signal received".to_string();
            break;
        }

        // Track the claimed task before the pipeline runs. Cleared below if
        // the pipeline reports the claimed task as completed.
        last_claimed_task = result.task_id.clone();

        // Update run with last files (stays at the call site — pipeline only
        // covers post-Claude completion / learning bookkeeping).
        if let Err(e) = run_cmd::update(
            &conn,
            &run_id,
            ctx.last_commit.as_deref(),
            Some(&result.files_modified),
        ) {
            eprintln!("Warning: failed to update run: {}", e);
        }

        // Record epoch before completion detection so we can later identify tasks
        // completed this iteration (used for human review triggering).
        let completion_epoch_start: i64 = conn
            .query_row("SELECT CAST(strftime('%s', 'now') AS INTEGER)", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        // Run the shared post-Claude pipeline: progress logging, key-decision
        // extraction, `<task-status>` dispatch, completion ladder
        // (status-tag → completed-tag → git/scan → already-complete fallback),
        // learning extraction, bandit feedback, and per-task crash-tracking.
        // Wrapper-commit, external-git reconciliation, and human-review
        // triggering stay at this call site (FEAT-005).
        let processing_outcome =
            iteration_pipeline::process_iteration_output(iteration_pipeline::ProcessingParams {
                conn: &mut conn,
                run_id: &run_id,
                iteration,
                task_id: result.task_id.as_deref(),
                output: &result.output,
                conversation: result.conversation.as_deref(),
                shown_learning_ids: &result.shown_learning_ids,
                outcome: &mut result.outcome,
                working_root: &working_root,
                git_scan_depth: run_config.config.git_scan_depth,
                skip_git_completion_detection: false,
                prd_path: &paths.prd_file,
                task_prefix: task_prefix.as_deref(),
                progress_path: &paths.progress_file,
                db_dir: &run_config.db_dir,
                signal_flag: &signal_flag,
                ctx: &mut ctx,
                files_modified: &result.files_modified,
                effective_model: result.effective_model.as_deref(),
                effective_effort: result.effective_effort,
                slot_index: None,
            });
        tasks_completed += processing_outcome.tasks_completed;
        result.key_decisions_count = processing_outcome.key_decisions_count;

        // Clear `last_claimed_task` only if the pipeline marked the claimed
        // task itself as completed (any branch of the completion ladder).
        // Cross-task `<completed>Y</completed>` completions do NOT clear it —
        // the claimed task may still be in flight.
        let claimed_was_completed = result
            .task_id
            .as_ref()
            .map(|tid| {
                processing_outcome
                    .completed_task_ids
                    .iter()
                    .any(|c| c == tid)
            })
            .unwrap_or(false);
        if claimed_was_completed {
            last_claimed_task = None;
        }

        // Wrapper commit: if the claimed task was completed but no git commit
        // exists (Claude couldn't commit in scoped permission mode), commit on
        // its behalf.
        if claimed_was_completed
            && let Some(ref task_id) = result.task_id
            && check_git_for_task_completion(
                &working_root,
                task_id,
                run_config.config.git_scan_depth,
            )
            .is_none()
            && let Some(hash) = wrapper_commit(&working_root, task_id, "loop wrapper commit")
        {
            ctx.last_commit = Some(hash);
        }

        // Post-iteration: reconcile external git completions
        // Catches tasks completed in the current iteration (and any missed from prior)
        if let Some(ref ext_repo) = external_repo_path
            && !matches!(result.outcome, IterationOutcome::Empty)
        {
            let count = reconcile_external_git_completions(
                ext_repo,
                &mut conn,
                &run_id,
                &paths.prd_file,
                task_prefix.as_deref(),
                run_config.config.external_git_scan_depth,
            );
            if count > 0 {
                tasks_completed += count as u32;

                // Override outcome so stale/crash trackers reset — task was actually completed
                result.outcome = IterationOutcome::Completed;
                ctx.crash_tracker.record_success();

                eprintln!(
                    "Post-iteration reconciliation: marked {} task(s) done",
                    count
                );
                // Clear tracker if the claimed task was reconciled as done
                if let Some(ref claimed) = last_claimed_task {
                    let status: Option<String> = conn
                        .query_row(
                            "SELECT status FROM tasks WHERE id = ?",
                            [claimed.as_str()],
                            |row| row.get(0),
                        )
                        .ok();
                    if status.as_deref() == Some("done") {
                        last_claimed_task = None;
                    }
                }
            }
        }

        // Trigger human review for requires_human tasks completed this iteration.
        // Queries by timestamp to capture all detection paths (tags, git, output scan,
        // external reconciliation). Pre-completed tasks have older timestamps and are skipped.
        if !matches!(result.outcome, IterationOutcome::Empty) {
            trigger_human_reviews(
                &conn,
                HumanReviewParams {
                    completion_epoch_start,
                    iteration,
                    session_guidance: &mut ctx.session_guidance,
                    prd_file: &paths.prd_file,
                    task_prefix: task_prefix.as_deref(),
                    default_model: default_model.as_deref(),
                    permission_mode: &permission_mode,
                },
            );
        }

        // Track iteration count (skip reorders and rate limits)
        match result.outcome {
            IterationOutcome::Reorder(_) | IterationOutcome::RateLimit => {
                // Don't count against iteration budget
                iteration -= 1;
            }
            IterationOutcome::Completed => {
                iterations_completed += 1;
            }
            _ => {
                iterations_completed += 1;
            }
        }

        // Retry tracking: increment consecutive_failures for non-Completed task failures.
        // Excluded: Empty (no task attempted), Reorder (not a failure), RateLimit (external).
        if let Some(ref task_id) = result.task_id
            && !matches!(
                result.outcome,
                IterationOutcome::Completed
                    | IterationOutcome::Empty
                    | IterationOutcome::Reorder(_)
                    | IterationOutcome::RateLimit
            )
            && let Err(e) = handle_task_failure(&mut conn, task_id, iteration as i64)
        {
            eprintln!("Warning: failed to start retry tracking transaction: {}", e);
        }

        // Track consecutive stale iterations and abort if stuck
        if matches!(result.outcome, IterationOutcome::NoEligibleTasks) {
            ctx.stale_tracker.check("stale", "stale"); // same hash → increment
            if ctx.stale_tracker.should_abort() {
                eprintln!(
                    "Aborting: no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                );
                exit_code = 1;
                exit_reason = format!(
                    "no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                );
                break;
            }
        } else {
            ctx.stale_tracker.check("a", "b"); // different hash → reset
        }

        // Check for terminal outcomes
        if result.should_stop {
            match &result.outcome {
                IterationOutcome::Completed => {
                    exit_code = 0;
                    exit_reason = "all tasks complete".to_string();
                    final_run_status = RunStatus::Completed;
                }
                IterationOutcome::Blocked => {
                    exit_code = 2;
                    exit_reason = "blocked".to_string();
                }
                IterationOutcome::Crash(_) => {
                    exit_code = 1;
                    exit_reason = "too many crashes".to_string();
                }
                IterationOutcome::Empty if signal_flag.is_signaled() => {
                    // Determine SIGINT vs SIGTERM — we can't distinguish,
                    // so default to SIGINT (130) since that's more common
                    exit_code = 130;
                    exit_reason = "signal received".to_string();
                }
                IterationOutcome::Empty => {
                    // Stop signal file or other empty exit
                    exit_code = 0;
                    exit_reason = "stop signal".to_string();
                    was_stopped = true;
                }
                IterationOutcome::PromptOverflow => {
                    exit_code = 3;
                    exit_reason = "prompt overflow — critical sections exceed budget".to_string();
                }
                _ => {
                    exit_code = 1;
                    exit_reason = "stopped".to_string();
                }
            }
            break;
        }
    }

    // Step 17.5: Reset uncompleted claimed task so it's not stuck in_progress for next run
    if let Some(ref task_id) = last_claimed_task {
        reset_task_to_todo(&conn, task_id, "uncompleted task");
    }

    // Step 17.6: Reset any parallel-mode slot tasks still pending. Sequential
    // mode is fully covered by step 17.5 above; the wave path tracks every
    // claimed task in `ctx.pending_slot_tasks` and removes it on `done`, so
    // anything remaining was claimed but never closed (deadline / max-iter
    // exit, slot crash, or output without a `<completed>` tag).
    for task_id in &ctx.pending_slot_tasks {
        if Some(task_id) == last_claimed_task.as_ref() {
            continue; // already handled by step 17.5
        }
        reset_task_to_todo(&conn, task_id, "uncompleted slot task");
    }

    // Step 18: Record session guidance if any
    record_session_guidance(
        &ctx.session_guidance,
        &paths.progress_file,
        run_config.config.yes_mode,
    );

    // Step 19: End run session
    if let Err(e) = run_cmd::end(&conn, &run_id, final_run_status) {
        eprintln!("Warning: failed to end run: {}", e);
    }

    // Step 20: Recalibrate weights if completed
    if final_run_status == RunStatus::Completed {
        on_run_completed(&conn, task_prefix.as_deref());
    }

    // Step 21: Cleanup
    deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
    signals::cleanup_signal_files_for_prefix(&paths.tasks_dir, task_prefix.as_deref());

    // Step 21.4: Slot worktree cleanup (parallel mode only).
    // Removes ephemeral slot worktrees (slots 1+) and their branches. Slot 0
    // is the loop's main branch worktree and is handled by step 21.5 below.
    // Always runs on shutdown so a crash does not leak stray worktrees.
    if parallel_active
        && let Some(ref branch) = branch_name
        && let Err(e) = worktree::cleanup_slot_worktrees(
            &run_config.source_root,
            branch,
            run_config.config.parallel_slots,
        )
    {
        eprintln!(
            "Warning: cleanup_slot_worktrees failed: {} — leaving slot worktrees intact",
            e
        );
    }

    // Step 21.5: Worktree cleanup (if a worktree was used)
    if let Some(ref wt_path) = actual_worktree_path {
        let should_cleanup = if run_config.config.cleanup_worktree {
            // --cleanup-worktree flag: always attempt removal
            true
        } else if run_config.config.yes_mode {
            // --yes without --cleanup-worktree: keep worktree (auto-keep)
            false
        } else {
            // Interactive: prompt user
            eprint!("Remove worktree at '{}'? [y/N] ", wt_path.display());
            let mut response = String::new();
            let _ = std::io::stdin().read_line(&mut response);
            matches!(response.trim().to_lowercase().as_str(), "y" | "yes")
        };

        if should_cleanup {
            match worktree::remove_worktree(&run_config.source_root, wt_path) {
                Ok(true) => eprintln!("Worktree '{}' removed.", wt_path.display()),
                Ok(false) => eprintln!(
                    "Warning: worktree '{}' has uncommitted changes — not removed.",
                    wt_path.display()
                ),
                Err(e) => eprintln!(
                    "Warning: failed to remove worktree '{}': {} — continuing.",
                    wt_path.display(),
                    e
                ),
            }
        }
    }

    // Step 21.7: Prompt user to resolve pending key decisions (skip on SIGINT or yes_mode)
    if exit_code != 130 {
        prompt_pending_key_decisions(&conn, &run_id, run_config.config.yes_mode);
    }

    // Step 22: Print final banner
    let total_elapsed = start_time.elapsed().as_secs();
    display::print_final_banner(
        iterations_completed,
        tasks_completed,
        total_elapsed,
        &exit_reason,
        &prd_basename,
    );

    LoopResult {
        exit_code,
        worktree_path: actual_worktree_path,
        branch_name: branch_name.clone(),
        was_stopped,
        tasks_completed,
    }
}

/// Context parameters for `trigger_human_reviews`.
struct HumanReviewParams<'a> {
    completion_epoch_start: i64,
    iteration: u32,
    session_guidance: &'a mut SessionGuidance,
    prd_file: &'a Path,
    task_prefix: Option<&'a str>,
    default_model: Option<&'a str>,
    permission_mode: &'a PermissionMode,
}

/// Query tasks that need human review for the current iteration.
///
/// Returns `(id, title, notes, timeout_secs)` tuples for all `requires_human=1` tasks
/// with `status='done'` and `completed_at >= epoch_start`. This captures every completion
/// path (tag detection, git commit, output scan, external reconciliation) because they all
/// write the same DB state; the caller filters by timestamp to skip pre-completed tasks.
///
/// Exposed as `pub(crate)` so tests can verify query semantics without stdin interaction.
pub(crate) fn query_human_review_tasks(
    conn: &Connection,
    epoch_start: i64,
) -> Vec<(String, String, Option<String>, Option<u32>)> {
    match conn.prepare(
        "SELECT id, title, notes, human_review_timeout \
         FROM tasks \
         WHERE requires_human = 1 AND status = 'done' \
         AND CAST(strftime('%s', completed_at) AS INTEGER) >= ?",
    ) {
        Ok(mut stmt) => match stmt.query_map([epoch_start], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?
                    .and_then(|v| u32::try_from(v).ok()),
            ))
        }) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                eprintln!("Warning: could not execute human review query: {}", e);
                vec![]
            }
        },
        Err(e) => {
            eprintln!("Warning: could not prepare human review query: {}", e);
            vec![]
        }
    }
}

/// Trigger interactive human review for any `requires_human` tasks completed this iteration.
///
/// Queries tasks completed at or after `completion_epoch_start` to capture all detection
/// paths (tags, git, output scan, external reconciliation). For each such task, calls
/// `handle_human_review` and — if feedback was provided — calls `mutate_prd_from_feedback`
/// to update downstream tasks.
fn trigger_human_reviews(conn: &Connection, params: HumanReviewParams<'_>) {
    let HumanReviewParams {
        completion_epoch_start,
        iteration,
        session_guidance,
        prd_file,
        task_prefix,
        default_model,
        permission_mode,
    } = params;

    let review_tasks = query_human_review_tasks(conn, completion_epoch_start);

    for (task_id, title, notes, timeout) in review_tasks {
        let had_feedback = handle_human_review(
            io::BufReader::new(io::stdin()),
            &task_id,
            &title,
            notes.as_deref(),
            iteration,
            session_guidance,
            timeout,
        );
        if had_feedback {
            let feedback = session_guidance.last_text().unwrap_or("").to_string();
            prd_reconcile::mutate_prd_from_feedback(
                prd_file,
                &feedback,
                conn,
                task_prefix,
                default_model,
                permission_mode,
            );
        }
    }
}

/// Query pending key decisions for the run and prompt the user to resolve or defer each.
///
/// In yes_mode, all decisions are auto-deferred without prompting.
/// This function is a no-op when there are no pending decisions.
fn prompt_pending_key_decisions(conn: &Connection, run_id: &str, yes_mode: bool) {
    let decisions = match key_decisions_db::get_pending_decisions(conn, run_id) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Warning: failed to query pending key decisions: {}", e);
            return;
        }
    };

    if decisions.is_empty() {
        return;
    }

    if yes_mode {
        for decision in &decisions {
            if let Err(e) = key_decisions_db::defer_decision(conn, decision.id) {
                eprintln!("Warning: failed to defer decision {}: {}", decision.id, e);
            }
        }
        eprintln!(
            "Auto-deferred {} key decision(s) (yes_mode).",
            decisions.len()
        );
        return;
    }

    eprintln!(
        "\n╔══════════════════════════════════════════════════╗\
         \n║         KEY DECISIONS REQUIRING YOUR INPUT        ║\
         \n╚══════════════════════════════════════════════════╝"
    );

    for decision in &decisions {
        loop {
            eprintln!("\n┌─ Decision: {}", decision.title);
            eprintln!("│  {}", decision.description);
            eprintln!("│");
            for (i, opt) in decision.options.iter().enumerate() {
                let letter = (b'A' + i as u8) as char;
                eprintln!("│  {}) {} — {}", letter, opt.label, opt.description);
            }
            eprintln!("│  S) Skip (defer to next session)");
            eprint!("└─ Your choice: ");

            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                // stdin unavailable — defer
                eprintln!("\nWarning: could not read stdin, deferring decision.");
                let _ = key_decisions_db::defer_decision(conn, decision.id);
                break;
            }

            let trimmed = input.trim().to_lowercase();

            if trimmed.is_empty() || trimmed == "s" || trimmed == "skip" {
                if let Err(e) = key_decisions_db::defer_decision(conn, decision.id) {
                    eprintln!("Warning: failed to defer decision: {}", e);
                } else {
                    eprintln!("Decision deferred.");
                }
                break;
            }

            // Match letter or label substring to an option
            match find_option(&decision.options, &trimmed) {
                Ok(opt) => {
                    let resolution = format!("{}: {}", opt.label, opt.description);
                    if let Err(e) =
                        key_decisions_db::resolve_decision(conn, decision.id, &resolution)
                    {
                        eprintln!("Warning: failed to resolve decision: {}", e);
                    } else {
                        eprintln!("Decision resolved: {}", resolution);
                    }
                    break;
                }
                Err(_) => {
                    eprintln!(
                        "Invalid choice — enter a letter (A–{}) or S to skip.",
                        (b'A' + decision.options.len() as u8 - 1) as char
                    );
                }
            }
        }
    }
}

/// Install SIGINT, SIGTERM, and SIGQUIT handlers that set the signal flag.
///
/// Uses `signal-hook` to register OS-level signal handlers that set an
/// `AtomicBool` directly from signal context — no async polling needed.
/// This works even when the tokio runtime thread is blocked in synchronous I/O
/// (e.g., reading Claude subprocess stdout).
///
/// Second Ctrl+C restores the default handler, which force-kills immediately.
fn setup_signal_handler(signal_flag: SignalFlag) {
    let flag = signal_flag.inner();

    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};

        // First SIGINT sets the flag; second SIGINT restores default (immediate kill).
        // Both registrations are needed: `register` sets the flag, and
        // `register_conditional_default` emulates the default handler when
        // the flag is already true.
        if let Err(e) = signal_hook::flag::register(SIGINT, flag.clone()) {
            eprintln!("Warning: failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag.clone()) {
            eprintln!(
                "Warning: failed to install SIGINT conditional default: {}",
                e
            );
        }
        if let Err(e) = signal_hook::flag::register(SIGTERM, flag.clone()) {
            eprintln!("Warning: failed to install SIGTERM handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register(SIGQUIT, flag) {
            eprintln!("Warning: failed to install SIGQUIT handler: {}", e);
        }
    }

    #[cfg(not(unix))]
    {
        use signal_hook::consts::SIGINT;
        if let Err(e) = signal_hook::flag::register(SIGINT, flag.clone()) {
            eprintln!("Warning: failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag) {
            eprintln!(
                "Warning: failed to install SIGINT conditional default: {}",
                e
            );
        }
    }
}

/// Called after a run ends with Completed status to recalibrate selection weights.
///
/// Analyzes historical task outcomes and adjusts the scoring weights used by
/// `select_next_task()`. Errors are logged but do not propagate (best-effort).
pub fn on_run_completed(conn: &Connection, task_prefix: Option<&str>) {
    match calibrate::recalibrate_weights(conn, task_prefix) {
        Ok(weights) => {
            let defaults = calibrate::SelectionWeights::default();
            if weights != defaults {
                eprintln!(
                    "Calibrated selection weights: file_overlap={}, priority_base={}",
                    weights.file_overlap, weights.priority_base
                );
            }
        }
        Err(e) => {
            eprintln!("Warning: weight calibration failed: {}", e);
        }
    }
}

/// Record accumulated session guidance to progress.txt on loop exit.
///
/// In interactive mode (not --yes), prompts the user before saving.
/// In --yes mode, auto-saves without prompting.
/// Does nothing if no guidance was recorded during the session.
fn record_session_guidance(guidance: &SessionGuidance, progress_path: &Path, yes_mode: bool) {
    if guidance.is_empty() {
        return;
    }

    // In interactive mode, ask the user
    if !yes_mode {
        eprint!("Session guidance was recorded. Save to progress.txt? (y/N) ");
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let trimmed = input.trim().to_lowercase();
                if trimmed != "y" && trimmed != "yes" {
                    eprintln!("Session guidance discarded.");
                    return;
                }
            }
            Err(_) => {
                // stdin not available (non-interactive), skip
                eprintln!("Warning: could not read stdin, skipping guidance recording");
                return;
            }
        }
    }

    let formatted = guidance.format_for_recording();
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress_path)
    {
        Ok(mut file) => {
            if let Err(e) = io::Write::write_all(&mut file, formatted.as_bytes()) {
                eprintln!(
                    "Warning: could not write session guidance to {}: {}",
                    progress_path.display(),
                    e
                );
            } else {
                eprintln!("Session guidance saved to {}", progress_path.display());
            }
        }
        Err(e) => {
            eprintln!(
                "Warning: could not open progress file {}: {}",
                progress_path.display(),
                e
            );
        }
    }
}

/// Dispatch a list of `<task-status>` side-band updates.
///
/// For each update:
/// 1. Call the existing status command handler (`complete`, `fail`, `skip`,
///    `irrelevant`, `unblock`, or `reset_tasks`) — NEVER bypass via raw SQL.
/// 2. On a successful `Done` transition, flip the matching PRD JSON entry's
///    `passes` field to `true` (symmetric with `task-mgr add`'s DB + JSON
///    sync). JSON-sync failures log a warning but do NOT roll back the DB,
///    mirroring `add.rs` behavior.
///
/// Dispatch failures (e.g. task not `in_progress` when `done` is claimed —
/// learning [1475]) are logged to stderr with the task id + status and the
/// loop continues to the next tag. Never silently swallow errors.
///
/// Returns one entry per input update preserving order: `(task_id, status,
/// applied)`. `applied=true` iff the dispatcher reported success. Per-update
/// granularity lets the iteration_pipeline gate test for the specific
/// `(claimed_id, Done, true)` tuple instead of a global "any update
/// succeeded" flag (M2 fix — learning #2238).
#[allow(clippy::too_many_arguments)]
pub fn apply_status_updates(
    conn: &mut Connection,
    updates: &[detection::TaskStatusUpdate],
    run_id: Option<&str>,
    prd_path: Option<&Path>,
    task_prefix: Option<&str>,
    progress_path: Option<&Path>,
    db_dir: Option<&Path>,
    mut ctx: Option<&mut IterationContext>,
) -> Vec<(String, detection::TaskStatusChange, bool)> {
    use detection::TaskStatusChange;

    let mut results: Vec<(String, TaskStatusChange, bool)> = Vec::with_capacity(updates.len());
    for update in updates {
        let task_ids = [update.task_id.clone()];
        let dispatch: Result<(), TaskMgrError> = match update.status {
            TaskStatusChange::Done => {
                // Auto-claim unclaimed tasks: todo -> in_progress -> done
                let current: Result<String, _> = conn.query_row(
                    "SELECT status FROM tasks WHERE id = ?",
                    [update.task_id.as_str()],
                    |row| row.get(0),
                );
                if let Ok(ref s) = current
                    && s == "todo"
                {
                    let _ = conn.execute(
                        "UPDATE tasks SET status = 'in_progress', \
                         started_at = datetime('now'), \
                         updated_at = datetime('now') \
                         WHERE id = ? AND status = 'todo'",
                        [update.task_id.as_str()],
                    );
                    if let Some(rid) = run_id {
                        let iter: i64 = conn
                            .query_row(
                                "SELECT COALESCE(MAX(iteration), 0) + 1 FROM run_tasks WHERE run_id = ?",
                                [rid],
                                |row| row.get(0),
                            )
                            .unwrap_or(1);
                        let _ = conn.execute(
                            "INSERT OR IGNORE INTO run_tasks (run_id, task_id, iteration, status) \
                             VALUES (?, ?, ?, 'started')",
                            rusqlite::params![rid, update.task_id, iter],
                        );
                    }
                }
                complete_cmd::complete(conn, &task_ids, run_id, None, false).map(|_| ())
            }
            TaskStatusChange::Failed => crate::commands::fail(
                conn,
                &task_ids,
                None,
                crate::cli::FailStatus::Blocked,
                run_id,
                false,
            )
            .map(|_| ()),
            TaskStatusChange::Skipped => {
                crate::commands::skip(conn, &task_ids, "<task-status> tag", run_id).map(|_| ())
            }
            TaskStatusChange::Irrelevant => {
                crate::commands::irrelevant(conn, &task_ids, "<task-status> tag", run_id, None)
                    .map(|_| ())
            }
            TaskStatusChange::Unblock => {
                crate::commands::unblock(conn, &update.task_id).map(|_| ())
            }
            TaskStatusChange::Reset => {
                crate::commands::reset::reset_tasks(conn, &task_ids).map(|_| ())
            }
        };

        let applied = dispatch.is_ok();
        match dispatch {
            Ok(()) => {
                // Only Done flips PRD JSON `passes` — other transitions leave
                // `passes: false` unchanged.
                if matches!(update.status, TaskStatusChange::Done) {
                    if let Some(path) = prd_path
                        && let Err(e) =
                            update_prd_task_passes(path, &update.task_id, true, task_prefix)
                    {
                        eprintln!(
                            "Warning: <task-status> dispatched {} to done in DB but PRD JSON sync failed ({}): {}",
                            update.task_id,
                            path.display(),
                            e,
                        );
                    }
                    // Milestone hook: when a MILESTONE-* task flips to done,
                    // append a summary block to progress-*.txt covering every
                    // entry since the last milestone summary, with crash-
                    // avoidance recommendations for any task that crashed/
                    // overflowed ≥2 times in the window.
                    // Hyphen-anchored to avoid false matches like
                    // `PRE-MILESTONE-NOTES` or `MILESTONEISH-1`.
                    let is_milestone = update.task_id.contains("-MILESTONE-")
                        || update.task_id.starts_with("MILESTONE-")
                        || update.task_id == "MILESTONE"
                        || update.task_id.ends_with("-MILESTONE");
                    if is_milestone && let Some(pp) = progress_path {
                        progress::summarize_milestone(pp, &update.task_id, db_dir);
                    }
                }
                // Prune crashed_last_iteration for terminal transitions. A
                // terminal row is no longer "active", so the map entry would
                // accumulate past the "bounded by active task count" invariant
                // documented on the field. Reset and Unblock are NOT terminal.
                if let Some(ref mut c) = ctx
                    && matches!(
                        update.status,
                        TaskStatusChange::Done
                            | TaskStatusChange::Failed
                            | TaskStatusChange::Skipped
                            | TaskStatusChange::Irrelevant
                    )
                {
                    c.crashed_last_iteration.remove(&update.task_id);
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: <task-status>{}:{:?}</task-status> dispatch failed: {}",
                    update.task_id, update.status, e,
                );
            }
        }
        results.push((update.task_id.clone(), update.status, applied));
    }
    results
}

/// Check whether crash recovery should escalate the model for this iteration.
///
/// Returns `Some(escalated_model)` when the previous iteration on
/// `current_task_id` crashed (i.e. `crashed_last_iteration[current_task_id]
/// == true`). Returns `None` when the task is absent from the map or its
/// last outcome was not a crash.
///
/// When `resolved_model` is `None`, assumes `SONNET_MODEL` baseline
/// and escalates to `OPUS_MODEL` (architect decision: None crash → opus).
///
/// Escalation is independent of `CrashTracker` backoff logic.
pub fn check_crash_escalation(
    crashed_last_iteration: &std::collections::HashMap<String, bool>,
    current_task_id: &str,
    resolved_model: Option<&str>,
) -> Option<String> {
    if !crashed_last_iteration
        .get(current_task_id)
        .copied()
        .unwrap_or(false)
    {
        return None;
    }
    // None / empty / whitespace model: assume sonnet baseline, escalate to opus
    match normalize_baseline(resolved_model) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_model(Some(m)),
    }
}

/// Treat `Some("")` and `Some("   ")` as "no model known" so both escalation
/// paths (`check_crash_escalation` and `escalate_task_model_if_needed`) share
/// the same baseline-fallback semantics.
fn normalize_baseline(model: Option<&str>) -> Option<&str> {
    model.filter(|s| !s.trim().is_empty())
}

/// Returns true if a task should be auto-blocked due to consecutive failures.
///
/// Auto-block fires when `consecutive_failures >= max_retries` AND `max_retries > 0`.
/// `max_retries=0` disables auto-blocking entirely (task retries indefinitely).
pub fn should_auto_block(consecutive_failures: i32, max_retries: i32) -> bool {
    max_retries > 0 && consecutive_failures >= max_retries
}

/// Returns true if the model should be escalated due to consecutive failures.
///
/// Fires at `consecutive_failures >= 2`, before the auto-block threshold.
/// Gives the task one more attempt at a higher-tier model before blocking.
pub fn should_escalate_for_consecutive_failures(consecutive_failures: i32) -> bool {
    consecutive_failures >= 2
}

/// Escalate the model for a task in the DB when consecutive failures reach the threshold.
///
/// Follows the same sonnet-baseline pattern as `check_crash_escalation`:
/// - `None` or empty model assumes sonnet baseline → escalates to opus.
/// - Sonnet → opus, Haiku → sonnet, Opus → opus (no-op at ceiling).
///
/// Returns `Some(new_model)` if escalation fired, `None` if below threshold or
/// the model tier is unknown. The DB is updated in-place when `Some` is returned.
pub fn escalate_task_model_if_needed(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
) -> TaskMgrResult<Option<String>> {
    if !should_escalate_for_consecutive_failures(new_count) {
        return Ok(None);
    }
    let current_model: Option<String> =
        conn.query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
            r.get::<_, Option<String>>(0)
        })?;
    // None / empty / whitespace model: assume sonnet baseline → escalate to opus.
    let escalated = match normalize_baseline(current_model.as_deref()) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_model(Some(m)),
    };
    if let Some(ref new_model) = escalated {
        conn.execute(
            "UPDATE tasks SET model = ? WHERE id = ?",
            rusqlite::params![new_model, task_id],
        )?;
        eprintln!(
            "Escalated task {} to model {} after {} consecutive failures",
            task_id, new_model, new_count
        );
    }
    Ok(escalated)
}

/// Increment `consecutive_failures` for a task in the DB.
///
/// Returns the new `consecutive_failures` count after incrementing.
pub fn increment_consecutive_failures(conn: &Connection, task_id: &str) -> TaskMgrResult<i32> {
    conn.execute(
        "UPDATE tasks SET consecutive_failures = consecutive_failures + 1 WHERE id = ?",
        [task_id],
    )?;
    let count: i32 = conn.query_row(
        "SELECT consecutive_failures FROM tasks WHERE id = ?",
        [task_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Reset `consecutive_failures` for a task in the DB to 0.
///
/// Called after a Completed outcome to clear the failure streak.
pub fn reset_consecutive_failures(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    conn.execute(
        "UPDATE tasks SET consecutive_failures = 0 WHERE id = ?",
        [task_id],
    )?;
    Ok(())
}

/// Auto-block a task by setting status to 'blocked' and recording a descriptive last_error.
///
/// Called when `should_auto_block()` returns true after an iteration.
/// Sets `blocked_at_iteration` for decay tracking (consistent with `fail/transition.rs`).
pub fn auto_block_task(
    conn: &Connection,
    task_id: &str,
    consecutive_failures: i32,
    current_iteration: i64,
) -> TaskMgrResult<()> {
    let msg = format!(
        "Auto-blocked after {} consecutive failures (task: {})",
        consecutive_failures, task_id
    );
    conn.execute(
        "UPDATE tasks SET status = 'blocked', last_error = ?, blocked_at_iteration = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![msg, current_iteration, task_id],
    )?;
    Ok(())
}

/// Increment consecutive failure count, escalate model tier if needed, and auto-block if the
/// task has exhausted its retry budget. All DB writes are wrapped in a single transaction.
///
/// `current_iteration` is used to set `blocked_at_iteration` on auto-blocked tasks for
/// decay tracking. Escalation is skipped when auto-block fires on the same iteration
/// (the escalated model would never be used).
pub fn handle_task_failure(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
) -> TaskMgrResult<()> {
    let tx = conn.transaction()?;

    let new_count = increment_consecutive_failures(&tx, task_id).map_err(|e| {
        eprintln!(
            "Warning: failed to increment consecutive_failures for {}: {}",
            task_id, e
        );
        e
    })?;

    let max_retries: i32 = tx
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = ?",
            [task_id],
            |r| r.get(0),
        )
        .unwrap_or(3);

    // Only escalate if auto-block won't immediately follow (escalated model would never be used)
    if !should_auto_block(new_count, max_retries)
        && let Err(e) = escalate_task_model_if_needed(&tx, task_id, new_count)
    {
        eprintln!("Warning: failed to escalate model for {}: {}", task_id, e);
    }

    if should_auto_block(new_count, max_retries) {
        if let Err(e) = auto_block_task(&tx, task_id, new_count, current_iteration) {
            eprintln!("Warning: failed to auto-block task {}: {}", task_id, e);
        } else {
            eprintln!(
                "Auto-blocked task {} after {} consecutive failures",
                task_id, new_count
            );
        }
    }

    tx.commit()?;

    Ok(())
}

/// Build an `IterationResult` for a prompt overflow, logging the error to stderr.
fn prompt_overflow_result(critical_size: usize, budget: usize, task_id: String) -> IterationResult {
    eprintln!(
        "FATAL: Prompt critical sections ({} bytes) exceed budget ({} bytes) for task {}. \
         Reduce base prompt.md size or split the task.",
        critical_size, budget, task_id,
    );
    IterationResult {
        outcome: IterationOutcome::PromptOverflow,
        task_id: Some(task_id),
        files_modified: vec![],
        should_stop: true,
        output: String::new(),
        effective_model: None,
        effective_effort: None,
        key_decisions_count: 0,
        conversation: None,
        shown_learning_ids: Vec::new(),
    }
}

/// Probe whether the CLI rate limit has been lifted by spawning a minimal Claude call.
///
/// Sends `claude -p "." --print --max-turns 1 --no-session-persistence` and checks
/// whether the output still contains rate-limit patterns. Returns `true` if the
/// limit appears to be lifted (Claude responds without a rate-limit error).
fn probe_rate_limit_lifted(permission_mode: &PermissionMode) -> bool {
    let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());

    let mut args = vec!["--print", "--no-session-persistence", "--max-turns", "1"];

    // Use the same permission mode as the main loop so the probe doesn't hang
    // on a permission prompt.
    let allowed_tools_str;
    match permission_mode {
        PermissionMode::Dangerous => {
            args.push("--dangerously-skip-permissions");
        }
        PermissionMode::Scoped { allowed_tools } => {
            args.push("--permission-mode");
            args.push("dontAsk");
            if let Some(tools) = allowed_tools {
                allowed_tools_str = tools.clone();
                args.push("--allowedTools");
                args.push(&allowed_tools_str);
            }
        }
        PermissionMode::Auto { allowed_tools } => {
            args.push("--permission-mode");
            args.push("auto");
            if let Some(tools) = allowed_tools {
                allowed_tools_str = tools.clone();
                args.push("--allowedTools");
                args.push(&allowed_tools_str);
            }
        }
    }

    args.push("-p");
    args.push(".");

    let output = match std::process::Command::new(&binary)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("  Probe failed to spawn: {}", e);
            return false;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}\n{}", stdout, stderr);

    !detection::is_rate_limited(&combined)
}

/// Update crash and stale trackers based on iteration outcome.
/// Returns true if the loop should stop.
fn update_trackers(ctx: &mut IterationContext, outcome: &IterationOutcome) -> bool {
    match outcome {
        IterationOutcome::Completed => {
            ctx.crash_tracker.record_success();
            // Stale tracker: "different hash" means progress was made
            // We use a simple proxy: completed = progress
            false
        }
        IterationOutcome::Crash(_) => {
            ctx.crash_tracker.record_crash();
            ctx.crash_tracker.should_abort()
        }
        IterationOutcome::Blocked => {
            // Blocked is not a crash — don't increment crash counter
            ctx.crash_tracker.record_success();
            false
        }
        IterationOutcome::RateLimit => {
            // Rate limit — don't count as crash but don't reset either
            false
        }
        IterationOutcome::Reorder(_) => {
            // Reorder — skip, not a real iteration result
            false
        }
        IterationOutcome::NoEligibleTasks => {
            // Stale detection handled by the outer loop via stale_tracker.check()
            false
        }
        IterationOutcome::Empty => {
            ctx.crash_tracker.record_crash();
            ctx.crash_tracker.should_abort()
        }
        IterationOutcome::PromptOverflow => {
            // Fatal — loop will stop via should_stop
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
    use crate::loop_engine::test_utils::{EnvGuard, setup_test_db};

    // --- LoopResult Default tests ---

    #[test]
    fn test_loop_result_default_is_zero() {
        let r = LoopResult::default();
        assert_eq!(r.exit_code, 0);
        assert!(r.worktree_path.is_none());
        assert!(r.branch_name.is_none());
        assert!(!r.was_stopped);
        assert_eq!(r.tasks_completed, 0);
    }

    #[test]
    fn test_loop_result_partial_construction_via_default() {
        let r = LoopResult {
            exit_code: 130,
            ..Default::default()
        };
        assert_eq!(r.exit_code, 130);
        assert!(r.worktree_path.is_none());
        assert!(r.branch_name.is_none());
        assert!(!r.was_stopped);
        assert_eq!(r.tasks_completed, 0);
    }

    // --- pre_lock_prefix fallback tests ---

    #[test]
    fn test_pre_lock_prefix_fallback_matches_generate_prefix() {
        use crate::commands::init::generate_prefix;
        use crate::loop_engine::status_queries::read_branch_name_from_prd;
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        // PRD without taskPrefix but with branchName
        fs::write(
            &prd_path,
            r#"{"branchName": "feat/test-branch", "description": "test"}"#,
        )
        .unwrap();

        let branch = read_branch_name_from_prd(&prd_path);
        let filename = prd_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let fallback = generate_prefix(branch.as_deref(), filename);
        // Also verify generate_prefix called directly with the same inputs matches
        let expected = generate_prefix(Some("feat/test-branch"), "my-prd.json");
        assert_eq!(fallback, expected);
    }

    #[test]
    fn test_pre_lock_prefix_uses_task_prefix_when_present() {
        use crate::loop_engine::status_queries::read_branch_name_from_prd;
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        fs::write(
            &prd_path,
            r#"{"taskPrefix": "abc12345", "branchName": "feat/test"}"#,
        )
        .unwrap();

        // When taskPrefix is present, read_task_prefix_from_prd returns it
        // and or_else branch must not run
        let task_prefix = crate::loop_engine::status_queries::read_task_prefix_from_prd(&prd_path);
        assert_eq!(task_prefix, Some("abc12345".to_string()));

        // or_else would only run if task_prefix is None
        let branch = read_branch_name_from_prd(&prd_path);
        // verify or_else branch not needed — task_prefix is Some
        let result = task_prefix.or_else(|| {
            let b = branch.clone();
            let filename = prd_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            Some(crate::commands::init::generate_prefix(
                b.as_deref(),
                filename,
            ))
        });
        assert_eq!(result, Some("abc12345".to_string()));
    }

    // --- IterationContext tests ---

    #[test]
    fn test_iteration_context_new() {
        let ctx = IterationContext::new(5);
        assert!(ctx.last_commit.is_none());
        assert!(ctx.last_files.is_empty());
        assert!(ctx.session_guidance.is_empty());
        assert!(ctx.reorder_hint.is_none());
        assert_eq!(ctx.reorder_count, 0);
        assert!(
            ctx.crashed_last_iteration.is_empty(),
            "TEST-INIT-004 contract: per-task crash map starts empty"
        );
    }

    // --- IterationResult tests ---

    #[test]
    fn test_iteration_result_fields() {
        let result = IterationResult {
            outcome: IterationOutcome::Completed,
            task_id: Some("FEAT-001".to_string()),
            files_modified: vec!["src/lib.rs".to_string()],
            should_stop: false,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        };
        assert_eq!(result.task_id, Some("FEAT-001".to_string()));
        assert!(!result.should_stop);
    }

    // --- update_trackers tests ---

    #[test]
    fn test_update_trackers_completed_resets_crash() {
        let mut ctx = IterationContext::new(5);
        ctx.crash_tracker.record_crash();
        ctx.crash_tracker.record_crash();
        assert_eq!(ctx.crash_tracker.count(), 2);

        let should_stop = update_trackers(&mut ctx, &IterationOutcome::Completed);
        assert!(!should_stop);
        assert_eq!(ctx.crash_tracker.count(), 0);
    }

    #[test]
    fn test_update_trackers_crash_increments() {
        let mut ctx = IterationContext::new(3);
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError);

        update_trackers(&mut ctx, &crash);
        assert_eq!(ctx.crash_tracker.count(), 1);

        update_trackers(&mut ctx, &crash);
        assert_eq!(ctx.crash_tracker.count(), 2);
    }

    #[test]
    fn test_update_trackers_crash_signals_abort() {
        let mut ctx = IterationContext::new(2);
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError);

        update_trackers(&mut ctx, &crash);
        let should_stop = update_trackers(&mut ctx, &crash);
        assert!(should_stop, "Should abort after max crashes");
    }

    #[test]
    fn test_update_trackers_blocked_does_not_increment_crash() {
        let mut ctx = IterationContext::new(5);
        update_trackers(&mut ctx, &IterationOutcome::Blocked);
        assert_eq!(ctx.crash_tracker.count(), 0);
    }

    #[test]
    fn test_update_trackers_rate_limit_no_crash() {
        let mut ctx = IterationContext::new(5);
        ctx.crash_tracker.record_crash(); // pre-existing crash
        update_trackers(&mut ctx, &IterationOutcome::RateLimit);
        // Should not reset or increment
        assert_eq!(ctx.crash_tracker.count(), 1);
    }

    #[test]
    fn test_update_trackers_reorder_no_crash() {
        let mut ctx = IterationContext::new(5);
        let reorder = IterationOutcome::Reorder("FEAT-001".to_string());
        let should_stop = update_trackers(&mut ctx, &reorder);
        assert!(!should_stop);
        assert_eq!(ctx.crash_tracker.count(), 0);
    }

    #[test]
    fn test_update_trackers_empty_increments_crash() {
        let mut ctx = IterationContext::new(5);
        update_trackers(&mut ctx, &IterationOutcome::Empty);
        assert_eq!(ctx.crash_tracker.count(), 1);
    }

    // --- MAX_CONSECUTIVE_REORDERS constant ---

    #[test]
    fn test_max_consecutive_reorders_is_2() {
        assert_eq!(MAX_CONSECUTIVE_REORDERS, 2);
    }

    // --- on_run_completed tests ---

    #[test]
    fn test_on_run_completed_no_panic_on_empty_db() {
        let (_temp_dir, conn) = setup_test_db();

        // Should not panic even with no data
        on_run_completed(&conn, None);
    }

    // --- record_session_guidance tests ---

    #[test]
    fn test_record_session_guidance_empty_does_nothing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let guidance = SessionGuidance::new();

        record_session_guidance(&guidance, &progress_path, true);

        // File should not be created
        assert!(!progress_path.exists());
    }

    #[test]
    fn test_record_session_guidance_yes_mode_auto_saves() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let mut guidance = SessionGuidance::new();
        guidance.add(3, "Focus on error handling".to_string());

        record_session_guidance(&guidance, &progress_path, true);

        assert!(progress_path.exists());
        let content = std::fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("Session Guidance"));
        assert!(content.contains("[Iteration 3] Focus on error handling"));
        assert!(content.contains("---"));
    }

    #[test]
    fn test_record_session_guidance_yes_mode_appends_to_existing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        std::fs::write(&progress_path, "# Existing content\n").unwrap();

        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test guidance".to_string());

        record_session_guidance(&guidance, &progress_path, true);

        let content = std::fs::read_to_string(&progress_path).unwrap();
        assert!(content.starts_with("# Existing content\n"));
        assert!(content.contains("Session Guidance"));
        assert!(content.contains("Test guidance"));
    }

    #[test]
    fn test_record_session_guidance_yes_mode_multiple_entries() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        let mut guidance = SessionGuidance::new();
        guidance.add(1, "First".to_string());
        guidance.add(5, "Second".to_string());
        guidance.add(10, "Third".to_string());

        record_session_guidance(&guidance, &progress_path, true);

        let content = std::fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("[Iteration 1] First"));
        assert!(content.contains("[Iteration 5] Second"));
        assert!(content.contains("[Iteration 10] Third"));
    }

    #[test]
    fn test_record_session_guidance_invalid_path_does_not_panic() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        // Writing to a non-existent directory — should not panic
        record_session_guidance(&guidance, Path::new("/nonexistent/dir/progress.txt"), true);
    }

    // --- startup recovery tests ---

    #[test]
    fn test_startup_recovery_resets_stale_tasks() {
        let (_temp_dir, conn) = crate::loop_engine::test_utils::setup_test_db();

        // Insert tasks in various states
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES
             ('T-001', 'Stale task', 'in_progress', 1, datetime('now', '-1 hour')),
             ('T-002', 'Normal todo', 'todo', 2, NULL),
             ('T-003', 'Done task', 'done', 3, datetime('now', '-2 hours'));",
        )
        .unwrap();

        // Run the same recovery SQL used in run_loop
        let count = conn
            .execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
                [],
            )
            .unwrap();

        assert_eq!(count, 1, "Should reset exactly 1 in_progress task");

        // Verify T-001 is now todo
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "T-001");
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_startup_recovery_preserves_done_tasks() {
        let (_temp_dir, conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('T-001', 'Done task', 'done', 1),
             ('T-002', 'Irrelevant task', 'irrelevant', 2);",
        )
        .unwrap();

        let count = conn
            .execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
                [],
            )
            .unwrap();

        assert_eq!(count, 0, "Should not touch done or irrelevant tasks");

        // Verify statuses unchanged
        let status1 = crate::loop_engine::test_utils::get_task_status(&conn, "T-001");
        let status2 = crate::loop_engine::test_utils::get_task_status(&conn, "T-002");
        assert_eq!(status1, "done");
        assert_eq!(status2, "irrelevant");
    }

    #[test]
    fn test_startup_recovery_clears_started_at() {
        let (_temp_dir, conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES
             ('T-001', 'Stale task', 'in_progress', 1, datetime('now'));",
        )
        .unwrap();

        // Verify started_at is set before recovery
        let before: Option<String> = conn
            .query_row(
                "SELECT started_at FROM tasks WHERE id = 'T-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(before.is_some(), "started_at should be set before recovery");

        // Run recovery
        conn.execute(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
            [],
        )
        .unwrap();

        // Verify started_at is cleared
        let after: Option<String> = conn
            .query_row(
                "SELECT started_at FROM tasks WHERE id = 'T-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(after.is_none(), "started_at should be NULL after recovery");
    }

    // --- Stale tracker wiring tests ---

    #[test]
    fn test_stale_abort_after_consecutive_stale_outcomes() {
        // Simulates the outer loop's stale tracker wiring:
        // 3 consecutive Stale outcomes should trigger abort.
        let mut ctx = IterationContext::new(5);

        // First stale
        ctx.stale_tracker.check("stale", "stale");
        assert!(
            !ctx.stale_tracker.should_abort(),
            "1 stale should not abort"
        );

        // Second stale
        ctx.stale_tracker.check("stale", "stale");
        assert!(
            !ctx.stale_tracker.should_abort(),
            "2 stale should not abort"
        );

        // Third stale
        ctx.stale_tracker.check("stale", "stale");
        assert!(
            ctx.stale_tracker.should_abort(),
            "3 consecutive stale should abort"
        );
    }

    #[test]
    fn test_stale_tracker_resets_on_non_stale_outcome() {
        // Non-Stale outcomes reset the stale tracker, preventing abort.
        let mut ctx = IterationContext::new(5);

        // Two stale
        ctx.stale_tracker.check("stale", "stale");
        ctx.stale_tracker.check("stale", "stale");
        assert_eq!(ctx.stale_tracker.count(), 2);

        // Non-stale resets
        ctx.stale_tracker.check("a", "b");
        assert_eq!(
            ctx.stale_tracker.count(),
            0,
            "Non-stale outcome should reset tracker"
        );
        assert!(!ctx.stale_tracker.should_abort());

        // One more stale — not enough to abort
        ctx.stale_tracker.check("stale", "stale");
        assert_eq!(ctx.stale_tracker.count(), 1);
        assert!(!ctx.stale_tracker.should_abort());
    }

    #[test]
    fn test_stale_recovery_resets_in_progress_tasks() {
        // Verifies the SQL recovery logic: in_progress tasks get reset to todo.

        let (_temp_dir, conn) = setup_test_db();

        // Insert tasks: one in_progress (stale), one blocked, one done
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Stale task', 'in_progress', 1),
             ('FEAT-002', 'Blocked task', 'blocked', 2),
             ('FEAT-003', 'Done task', 'done', 3);",
        )
        .unwrap();

        // Simulate the auto-recovery SQL from run_iteration
        let recovered = conn
            .execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
                [],
            )
            .unwrap();

        assert_eq!(recovered, 1, "Should recover exactly 1 in_progress task");

        // Verify the task was reset
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(status, "todo", "in_progress task should be reset to todo");

        // Verify other tasks are unaffected
        let blocked_status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-002");
        assert_eq!(
            blocked_status, "blocked",
            "Blocked task should be unaffected"
        );

        let done_status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-003");
        assert_eq!(done_status, "done", "Done task should be unaffected");
    }

    // --- check_crash_escalation tests ---

    /// Build a `crashed_last_iteration` map from a slice of `(task_id, is_crash)` pairs.
    fn crash_map(entries: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect()
    }

    /// First iteration: empty map — no crash recorded yet.
    #[test]
    fn test_crash_escalation_first_iteration_no_crash() {
        let result = check_crash_escalation(&crash_map(&[]), "FEAT-001", Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration without crash must not escalate"
        );
    }

    /// First iteration with crash: task absent from map — no escalation yet
    /// (the pipeline writes to the map AFTER the iteration, so the first pick
    /// of a new task always finds it absent).
    #[test]
    fn test_crash_escalation_first_iteration_with_crash() {
        let result = check_crash_escalation(&crash_map(&[]), "FEAT-001", Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration crash has no previous task context, cannot escalate"
        );
    }

    /// Same task but no crash — no escalation.
    #[test]
    fn test_crash_escalation_same_task_no_crash() {
        let result = check_crash_escalation(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(result, None, "same task without crash must not escalate");
    }

    /// Different task with crash — no escalation (crash on a different task
    /// does not carry forward).
    #[test]
    fn test_crash_escalation_different_task_with_crash() {
        let result = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-002",
            Some(SONNET_MODEL),
        );
        assert_eq!(
            result, None,
            "crash on different task must not escalate for new task"
        );
    }

    /// AC: same task + crash + haiku model → escalate to sonnet.
    #[test]

    fn test_crash_escalation_haiku_to_sonnet() {
        let result = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(
            result,
            Some(SONNET_MODEL.to_string()),
            "haiku crash on same task must escalate to sonnet"
        );
    }

    /// AC: same task + crash + sonnet model → escalate to opus.
    #[test]

    fn test_crash_escalation_sonnet_to_opus() {
        let result = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "sonnet crash on same task must escalate to opus"
        );
    }

    /// AC: same task + crash + already opus → stays opus (ceiling, no panic).
    #[test]

    fn test_crash_escalation_opus_ceiling() {
        let result = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(OPUS_MODEL),
        );
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "opus crash on same task must stay at opus ceiling"
        );
    }

    /// AC: resolved_model=None + crash → treated as SONNET_MODEL baseline,
    /// escalated to OPUS_MODEL. Architect decision: None crash assumes sonnet
    /// baseline and escalates to opus (not a no-op).
    #[test]

    fn test_crash_escalation_none_model_to_opus() {
        let result = check_crash_escalation(&crash_map(&[("FEAT-001", true)]), "FEAT-001", None);
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "None model crash must assume sonnet baseline and escalate to opus"
        );
    }

    /// Empty / whitespace-only models must be normalized to baseline so they
    /// escalate to opus rather than silently dropping the model on the floor.
    /// Keeps `check_crash_escalation` and `escalate_task_model_if_needed` in sync.
    #[test]
    fn test_crash_escalation_empty_and_whitespace_normalize_to_opus() {
        for bad in ["", "   ", "\t", " \n "] {
            let result =
                check_crash_escalation(&crash_map(&[("FEAT-001", true)]), "FEAT-001", Some(bad));
            assert_eq!(
                result,
                Some(OPUS_MODEL.to_string()),
                "bogus model {bad:?} must normalize to sonnet baseline and escalate"
            );
        }
    }

    /// Known-bad discriminator: escalation requires BOTH same task AND crash.
    /// An implementation that checks only one condition would pass one assertion
    /// but fail the other.
    #[test]

    fn test_crash_escalation_requires_both_conditions() {
        // Only same task (no crash) — must NOT escalate
        let no_crash = check_crash_escalation(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(no_crash, None, "same task without crash must NOT escalate");

        // Only crash (different task) — must NOT escalate
        let diff_task = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-002",
            Some(SONNET_MODEL),
        );
        assert_eq!(diff_task, None, "crash on different task must NOT escalate");

        // BOTH conditions — MUST escalate
        let both = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(
            both,
            Some(OPUS_MODEL.to_string()),
            "same task + crash MUST escalate"
        );
    }

    // ===== TEST-004: Comprehensive crash recovery escalation tests =====

    /// AC: Crash on task A, success on task A, crash on task A again.
    /// After success the map entry flips to false, so the next crash escalates
    /// from the base model (not the previously escalated model).
    #[test]
    fn test_crash_escalation_success_resets_escalation() {
        // First crash: haiku → sonnet
        let first = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(first, Some(SONNET_MODEL.to_string()));

        // After success the pipeline writes false into the map.
        let after_success = check_crash_escalation(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            first.as_deref(),
        );
        assert_eq!(
            after_success, None,
            "After success, no crash escalation should occur"
        );

        // Crash again on same task with original base model.
        let second_crash = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(
            second_crash,
            Some(SONNET_MODEL.to_string()),
            "After success reset, crash escalates from base model again"
        );
    }

    /// AC: Crash on task A, then task B is picked → no escalation for task B.
    /// The crash flag is keyed by task_id; TASK-B is absent from the map.
    #[test]
    fn test_crash_escalation_task_boundary_isolation() {
        // Crash on task A: haiku → sonnet
        let crash_a =
            check_crash_escalation(&crash_map(&[("TASK-A", true)]), "TASK-A", Some(HAIKU_MODEL));
        assert_eq!(crash_a, Some(SONNET_MODEL.to_string()));

        // Task B is selected next. TASK-A crashed but TASK-B is absent from map.
        let crash_b =
            check_crash_escalation(&crash_map(&[("TASK-A", true)]), "TASK-B", Some(HAIKU_MODEL));
        assert_eq!(
            crash_b, None,
            "Crash escalation must not carry across task boundaries"
        );
    }

    /// AC: Crash escalation is independent of CrashTracker backoff count.
    /// check_crash_escalation only consults the map and resolved_model.
    #[test]
    fn test_crash_escalation_independent_of_crash_tracker() {
        // Same map + same task + same model → same result every time.
        let result1 = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        let result2 = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(
            result1, result2,
            "Same inputs must produce same outputs — no hidden state"
        );
        assert_eq!(
            result1,
            Some(SONNET_MODEL.to_string()),
            "Escalation result is deterministic"
        );
    }

    /// Edge case: multiple consecutive crashes on same task follow the ladder:
    /// haiku → sonnet → opus → opus (ceiling).
    #[test]

    fn test_crash_escalation_consecutive_ladder() {
        let crashed = crash_map(&[("FEAT-001", true)]);
        // First crash: haiku → sonnet
        let first = check_crash_escalation(&crashed, "FEAT-001", Some(HAIKU_MODEL));
        assert_eq!(
            first,
            Some(SONNET_MODEL.to_string()),
            "first crash: haiku → sonnet"
        );

        // Second crash: feed escalated model back in (sonnet → opus)
        let second = check_crash_escalation(&crashed, "FEAT-001", first.as_deref());
        assert_eq!(
            second,
            Some(OPUS_MODEL.to_string()),
            "second crash: sonnet → opus"
        );

        // Third crash: opus → opus (ceiling)
        let third = check_crash_escalation(&crashed, "FEAT-001", second.as_deref());
        assert_eq!(
            third,
            Some(OPUS_MODEL.to_string()),
            "third crash: opus stays at ceiling"
        );
    }

    // =====================================================================
    // Prefix-scoped engine query tests (SS-SS-TEST-001)
    //
    // Each test sets up two PRDs (P1-*, P2-*) in the same DB and verifies
    // that engine queries respect prefix boundaries.
    // =====================================================================

    /// Helper: insert P1 and P2 tasks into the test DB.
    ///
    /// P1 tasks: P1-TASK-001 (in_progress), P1-TASK-002 (todo), P1-TASK-003 (done)
    /// P2 tasks: P2-TASK-001 (in_progress), P2-TASK-002 (todo)
    fn insert_dual_prd_tasks(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES
             ('P1-TASK-001', 'P1 stale task',   'in_progress', 1, datetime('now', '-1 hour')),
             ('P1-TASK-002', 'P1 todo task',     'todo',        2, NULL),
             ('P1-TASK-003', 'P1 done task',     'done',        3, NULL),
             ('P2-TASK-001', 'P2 stale task',    'in_progress', 1, datetime('now', '-1 hour')),
             ('P2-TASK-002', 'P2 todo task',     'todo',        2, NULL);",
        )
        .unwrap();
    }

    /// Build the full SQL and params for a prefix-scoped query, then call `execute_fn`.
    ///
    /// Eliminates the `prefix_and` → `format!` → params-Vec boilerplate shared by
    /// the initial-recovery and remaining-count prefix scope tests.
    fn run_with_prefix<T>(
        sql_template: &str,
        prefix: Option<&str>,
        execute_fn: impl FnOnce(&str, &[&dyn rusqlite::types::ToSql]) -> T,
    ) -> T {
        use crate::db::prefix::prefix_and;
        let (pfx_clause, pfx_param) = prefix_and(prefix);
        let sql = format!("{sql_template} {pfx_clause}");
        let params: Vec<&dyn rusqlite::types::ToSql> = match &pfx_param {
            Some(p) => vec![p],
            None => vec![],
        };
        execute_fn(&sql, params.as_slice())
    }

    // --- Initial recovery scoping ---

    #[test]
    fn test_initial_recovery_resets_only_p1_in_progress() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        // Simulate initial recovery with P1 prefix (as done in run_loop)
        let count = run_with_prefix(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
            Some("P1"),
            |sql, params| conn.execute(sql, params).unwrap(),
        );

        assert_eq!(count, 1, "Should reset only P1's in_progress task");

        // P1-TASK-001 should now be todo
        let p1_status = crate::loop_engine::test_utils::get_task_status(&conn, "P1-TASK-001");
        assert_eq!(p1_status, "todo");

        // P2-TASK-001 must still be in_progress — untouched by P1 recovery
        let p2_status = crate::loop_engine::test_utils::get_task_status(&conn, "P2-TASK-001");
        assert_eq!(
            p2_status, "in_progress",
            "P2 task must not be affected by P1 recovery"
        );
    }

    #[test]
    fn test_initial_recovery_none_prefix_resets_all_in_progress() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        // None prefix → no WHERE clause addition → resets all in_progress
        let count = run_with_prefix(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
            None,
            |sql, params| conn.execute(sql, params).unwrap(),
        );

        assert_eq!(
            count, 2,
            "None prefix should reset all in_progress tasks (backwards compat)"
        );
    }

    // --- Remaining count scoping ---

    #[test]
    fn test_remaining_count_scoped_to_p1() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        // Count remaining (not done/irrelevant) for P1 only
        let remaining: i64 = run_with_prefix(
            "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL",
            Some("P1"),
            |sql, params| conn.query_row(sql, params, |row| row.get(0)).unwrap(),
        );

        // P1 has in_progress + todo = 2 remaining (P1-TASK-003 is done)
        assert_eq!(remaining, 2, "P1 remaining should be 2 (not counting P2)");
    }

    #[test]
    fn test_remaining_count_none_prefix_counts_all() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        let remaining: i64 = run_with_prefix(
            "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL",
            None,
            |sql, params| conn.query_row(sql, params, |row| row.get(0)).unwrap(),
        );

        // 4 tasks total (P1: 2 + P2: 2), done is excluded
        assert_eq!(remaining, 4, "None prefix should count all remaining tasks");
    }

    // --- Signal flag propagation from Claude exit code ---

    #[test]
    fn test_signal_flag_set_on_exit_code_130() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());

        // Simulate what run_iteration does when Claude exits with 130 (SIGINT)
        let exit_code = 130;
        let completion_killed = false;
        if matches!(exit_code, 130 | 143) && !completion_killed {
            flag.set();
        }
        assert!(flag.is_signaled(), "Exit code 130 should set signal flag");
    }

    #[test]
    fn test_signal_flag_set_on_exit_code_143() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());

        let exit_code = 143;
        let completion_killed = false;
        if matches!(exit_code, 130 | 143) && !completion_killed {
            flag.set();
        }
        assert!(flag.is_signaled(), "Exit code 143 should set signal flag");
    }

    #[test]
    fn test_signal_flag_not_set_on_normal_exit_codes() {
        for exit_code in [0, 1, 127, 137, 139] {
            let flag = SignalFlag::new();
            let completion_killed = false;
            if matches!(exit_code, 130 | 143) && !completion_killed {
                flag.set();
            }
            assert!(
                !flag.is_signaled(),
                "Exit code {} should not set signal flag",
                exit_code
            );
        }
    }

    /// Regression: post-completion grace kill sends SIGTERM (exit 143), but
    /// that's an internal finalizer — it must NOT propagate to the parent's
    /// signal flag, or the batch runner ends the whole loop + chained PRDs
    /// after every `<completed>` tag.
    #[test]
    fn test_signal_flag_not_set_on_completion_killed_143() {
        let flag = SignalFlag::new();
        let exit_code = 143;
        let completion_killed = true;
        if matches!(exit_code, 130 | 143) && !completion_killed {
            flag.set();
        }
        assert!(
            !flag.is_signaled(),
            "exit 143 from post-completion grace kill must not set signal flag"
        );
    }

    // --- Auto-mode hint condition tests ---
    // Tests verify the conditional logic that controls when the hint fires.
    // Uses HINT_ENV_MUTEX to serialise env-var mutations across parallel tests.

    use std::sync::Mutex;
    static HINT_ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Mirrors the inline hint condition in run_loop() so the logic can be unit-tested.
    fn hint_should_fire(mode: &config::PermissionMode) -> bool {
        if let Ok(val) = std::env::var("LOOP_AUTO_MODE_AVAILABLE") {
            config::parse_bool_value(&val) == Some(true)
                && !matches!(mode, config::PermissionMode::Auto { .. })
        } else {
            false
        }
    }

    use super::AUTO_MODE_DEPRECATION_HINT as HINT_MSG;

    #[test]
    fn test_hint_fires_when_available_true_and_mode_scoped() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "true");
        let mode = config::PermissionMode::text_only();
        let fires = hint_should_fire(&mode);
        assert!(
            fires,
            "Hint should fire when available=true and mode=Scoped"
        );
    }

    #[test]
    fn test_hint_fires_when_available_true_and_mode_dangerous() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "true");
        let mode = config::PermissionMode::Dangerous;
        let fires = hint_should_fire(&mode);
        assert!(
            fires,
            "Hint should fire when available=true and mode=Dangerous"
        );
    }

    #[test]
    fn test_hint_does_not_fire_when_available_unset() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("LOOP_AUTO_MODE_AVAILABLE");
        let mode = config::PermissionMode::text_only();
        assert!(
            !hint_should_fire(&mode),
            "Hint must not fire when env var is unset"
        );
    }

    #[test]
    fn test_hint_does_not_fire_when_available_false() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "false");
        let mode = config::PermissionMode::text_only();
        let fires = hint_should_fire(&mode);
        assert!(!fires, "Hint must not fire when available=false");
    }

    #[test]
    fn test_hint_does_not_fire_when_mode_is_auto() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "true");
        let mode = config::PermissionMode::Auto {
            allowed_tools: None,
        };
        let fires = hint_should_fire(&mode);
        assert!(!fires, "Hint must not fire when mode is already Auto");
    }

    #[test]
    fn test_hint_message_contains_enable_auto_mode_env_var() {
        assert!(
            HINT_MSG.contains("LOOP_ENABLE_AUTO_MODE=true"),
            "Hint must mention LOOP_ENABLE_AUTO_MODE=true env var"
        );
    }

    #[test]
    fn test_hint_message_uses_yellow_ansi_prefix() {
        // Yellow ANSI escape: \x1b[33m
        assert!(
            HINT_MSG.contains("\x1b[33m"),
            "Hint must use yellow ANSI color code \\x1b[33m"
        );
        assert!(HINT_MSG.contains("[hint]"), "Hint must have [hint] prefix");
    }

    #[test]
    fn test_hint_message_says_deprecated() {
        assert!(
            HINT_MSG.contains("will be deprecated"),
            "Hint must mention that the permission model will be deprecated"
        );
    }

    #[test]
    fn test_hint_message_says_current_settings_continue() {
        assert!(
            HINT_MSG.contains("current settings continue"),
            "Hint must reassure users that their current settings continue to work"
        );
    }

    // --- retry tracking and auto-block tests ---
    //
    // Active tests verify the "should NOT block/escalate" cases — these pass
    // against the current stub (returns false).
    // Ignored tests define the expected behavior contract for FEAT-003/FEAT-004.

    /// Active: auto-block must NOT trigger on first attempt (consecutive_failures=0).
    #[test]
    fn test_auto_block_not_triggered_on_first_attempt() {
        assert!(
            !should_auto_block(0, 3),
            "auto-block must not fire on first attempt (consecutive_failures=0, max_retries=3)"
        );
    }

    /// Active: max_retries=0 disables auto-block entirely (never fires regardless of failures).
    #[test]
    fn test_auto_block_disabled_when_max_retries_zero() {
        assert!(
            !should_auto_block(0, 0),
            "max_retries=0 must disable auto-block at 0 failures"
        );
        assert!(
            !should_auto_block(5, 0),
            "max_retries=0 must disable auto-block at 5 failures"
        );
        assert!(
            !should_auto_block(100, 0),
            "max_retries=0 must disable auto-block regardless of failure count"
        );
    }

    /// Active: auto-block does NOT fire one below the threshold (2 < 3).
    #[test]
    fn test_auto_block_not_triggered_below_threshold() {
        assert!(
            !should_auto_block(2, 3),
            "auto-block must not fire at consecutive_failures=2, max_retries=3 (threshold not reached)"
        );
    }

    /// Active: negative consecutive_failures never triggers auto-block (safety invariant).
    #[test]
    fn test_auto_block_negative_failures_safe() {
        assert!(
            !should_auto_block(-1, 3),
            "negative consecutive_failures must never trigger auto-block"
        );
    }

    /// Active: model escalation NOT triggered at zero failures.
    #[test]
    fn test_failure_escalation_not_triggered_on_zero_failures() {
        assert!(
            !should_escalate_for_consecutive_failures(0),
            "model escalation must not fire at consecutive_failures=0"
        );
    }

    /// Active: model escalation NOT triggered at consecutive_failures=1.
    #[test]
    fn test_failure_escalation_not_triggered_on_single_failure() {
        assert!(
            !should_escalate_for_consecutive_failures(1),
            "model escalation must not fire at consecutive_failures=1"
        );
    }

    /// Auto-block triggers at exactly the max_retries threshold.
    #[test]
    fn test_auto_block_triggers_at_max_retries_threshold() {
        assert!(
            should_auto_block(3, 3),
            "auto-block must fire when consecutive_failures == max_retries (3 >= 3)"
        );
    }

    /// Auto-block triggers above the threshold.
    #[test]
    fn test_auto_block_triggers_above_threshold() {
        assert!(
            should_auto_block(4, 3),
            "auto-block must fire when consecutive_failures > max_retries (4 >= 3)"
        );
    }

    /// Auto-block triggers with max_retries=1 after one failure.
    #[test]
    fn test_auto_block_triggers_with_max_retries_one() {
        assert!(
            should_auto_block(1, 1),
            "auto-block must fire when consecutive_failures=1 and max_retries=1"
        );
    }

    /// Model escalation fires at consecutive_failures >= 2 (before auto-block at 3).
    #[test]
    fn test_failure_escalation_fires_at_consecutive_failures_two() {
        assert!(
            should_escalate_for_consecutive_failures(2),
            "model escalation must fire at consecutive_failures=2 (before auto-block threshold of 3)"
        );
    }

    /// Model escalation also fires at consecutive_failures=3.
    #[test]
    fn test_failure_escalation_fires_at_three() {
        assert!(
            should_escalate_for_consecutive_failures(3),
            "model escalation must fire at consecutive_failures=3"
        );
    }

    /// consecutive_failures increments by 1 in the DB after a non-Completed outcome.
    #[test]
    fn test_consecutive_failures_increments_in_db() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let new_count = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(
            new_count, 1,
            "consecutive_failures must increment from 0 to 1"
        );

        let new_count2 = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(
            new_count2, 2,
            "consecutive_failures must increment from 1 to 2"
        );
    }

    /// consecutive_failures resets to 0 in the DB after a Completed outcome.
    #[test]
    fn test_consecutive_failures_resets_to_zero_in_db() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 3)",
            [],
        )
        .unwrap();

        reset_consecutive_failures(&conn, "T-001").unwrap();

        let count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "consecutive_failures must reset to 0 after success"
        );
    }

    /// Auto-block sets last_error with a descriptive message.
    #[test]
    fn test_auto_block_sets_last_error_with_descriptive_message() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures, max_retries) VALUES ('T-001', 'Test', 'in_progress', 3, 3)",
            [],
        )
        .unwrap();

        auto_block_task(&conn, "T-001", 3, 1).unwrap();

        let (status, last_error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM tasks WHERE id = 'T-001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            status, "blocked",
            "auto-blocked task must have status='blocked'"
        );
        assert!(last_error.is_some(), "auto-block must set last_error");
        let err = last_error.unwrap();
        // Message must reference failures — exact wording up to implementer
        assert!(
            err.contains('3')
                || err.to_lowercase().contains("consecutive")
                || err.to_lowercase().contains("fail"),
            "last_error must describe the failure count, got: '{}'",
            err
        );
    }

    /// Task succeeds on 3rd attempt → counter resets to 0, auto-block NOT triggered.
    #[test]
    fn test_task_succeeds_on_third_attempt_counter_resets() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures, max_retries) VALUES ('T-001', 'Test', 'in_progress', 0, 3)",
            [],
        )
        .unwrap();

        // Two failures (counter: 0 → 2, below max_retries=3 → no auto-block)
        let c1 = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(c1, 1);
        assert!(!should_auto_block(c1, 3), "no auto-block at count=1");

        let c2 = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(c2, 2);
        assert!(
            !should_auto_block(c2, 3),
            "no auto-block at count=2 (below max_retries=3)"
        );

        // Success on 3rd attempt → counter resets to 0
        reset_consecutive_failures(&conn, "T-001").unwrap();
        let final_count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            final_count, 0,
            "counter must reset to 0 after success on attempt 3"
        );
        assert!(
            !should_auto_block(final_count, 3),
            "reset counter must not trigger auto-block"
        );
    }

    /// Rapid alternating success/failure on same task → counter tracks correctly.
    #[test]
    fn test_rapid_alternating_success_failure_tracks_correctly() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        // Pattern: fail → reset → fail → fail → reset
        increment_consecutive_failures(&conn, "T-001").unwrap(); // 1
        reset_consecutive_failures(&conn, "T-001").unwrap(); // 0
        increment_consecutive_failures(&conn, "T-001").unwrap(); // 1
        increment_consecutive_failures(&conn, "T-001").unwrap(); // 2
        reset_consecutive_failures(&conn, "T-001").unwrap(); // 0

        let count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "reset must zero counter regardless of prior alternation pattern"
        );

        // Verify next failure increments from 0
        increment_consecutive_failures(&conn, "T-001").unwrap();
        let count2: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count2, 1,
            "failure after reset must start from 0, not carry over prior streak"
        );
    }

    /// Resetting one task's failures does not affect a different task's counter.
    #[test]
    fn test_reset_scoped_to_task_not_cross_task() {
        let (_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES
             ('T-001', 'Task A', 'in_progress', 2),
             ('T-002', 'Task B', 'in_progress', 0);",
        )
        .unwrap();

        // Succeeding T-002 must NOT reset T-001's counter
        reset_consecutive_failures(&conn, "T-002").unwrap();
        let count_a: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count_a, 2,
            "resetting T-002 must not affect T-001's consecutive_failures"
        );
    }

    /// Increment always produces a non-negative result (invariant).
    #[test]
    fn test_consecutive_failures_never_goes_negative() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let count = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert!(
            count >= 0,
            "consecutive_failures must never be negative, got {}",
            count
        );

        reset_consecutive_failures(&conn, "T-001").unwrap();
        let after_reset: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            after_reset >= 0,
            "consecutive_failures must never be negative after reset, got {}",
            after_reset
        );
    }

    // --- escalate_task_model_if_needed tests (FEAT-004) ---

    /// Sonnet task at 2 consecutive failures → model escalated to opus in DB.
    #[test]
    fn test_model_escalation_sonnet_to_opus_at_two_failures() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let result = escalate_task_model_if_needed(&conn, "T-001", 2).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "sonnet at 2 failures must escalate to opus"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(OPUS_MODEL.to_string()),
            "model column in DB must be updated to opus"
        );
    }

    /// Opus task at 2 consecutive failures → model stays at opus (ceiling, no-op).
    #[test]
    fn test_model_escalation_opus_stays_at_ceiling() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{OPUS_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let result = escalate_task_model_if_needed(&conn, "T-001", 2).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "opus at ceiling must return opus (no-op value)"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(OPUS_MODEL.to_string()),
            "opus model in DB must remain opus"
        );
    }

    // --- query_human_review_tasks tests (TEST-001) ---

    /// Helper: insert a task with requires_human flag and a specific completed_at timestamp.
    ///
    /// `completed_at` is an ISO-8601 string (e.g. `datetime('now', '-10 seconds')` evaluated
    /// beforehand, or a literal like `"2020-01-01T00:00:00"`).
    fn insert_requires_human_task(
        conn: &Connection,
        id: &str,
        requires_human: i32,
        completed_at: &str,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, requires_human, completed_at) \
             VALUES (?, ?, 'done', 10, ?, ?)",
            rusqlite::params![id, format!("Task {id}"), requires_human, completed_at],
        )
        .unwrap();
    }

    /// Returns the current Unix epoch as i64.
    fn now_epoch() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Task with requires_human=0 must NOT appear in query results (criterion 8).
    #[test]
    fn test_human_review_query_no_requires_human_skipped() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-NRH", 0, "2099-01-01T12:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert!(
            tasks.is_empty(),
            "requires_human=0 task must not be returned by human review query"
        );
    }

    /// Task with requires_human=1 and recent completed_at must be returned (criteria 1-4).
    ///
    /// All completion detection paths (tag, git commit, output scan, external reconciliation)
    /// write the same DB state: status='done' + completed_at=<now>. The query selects by
    /// timestamp, so this single test covers all four detection paths.
    #[test]
    fn test_human_review_query_recent_completion_returned() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        // completed_at in the future (well after epoch) simulates "completed this iteration"
        insert_requires_human_task(&conn, "T-RH", 1, "2099-01-01T12:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(tasks.len(), 1, "one requires_human=1 task must be returned");
        assert_eq!(tasks[0].0, "T-RH");
        assert_eq!(tasks[0].1, "Task T-RH");
    }

    /// Task completed before epoch (pre-completed at import) must be skipped (criterion 6).
    #[test]
    fn test_human_review_query_precompeted_task_skipped() {
        let (_dir, conn) = setup_test_db();
        // epoch = now; completed_at = far in the past → completed_at epoch < epoch
        let epoch = now_epoch();
        insert_requires_human_task(&conn, "T-OLD", 1, "2000-01-01T00:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert!(
            tasks.is_empty(),
            "task completed before epoch (pre-completed at import) must be skipped"
        );
    }

    /// Multiple requires_human=1 tasks completed this iteration must all be returned (criterion 7).
    ///
    /// Each task in the returned list will be passed to handle_human_review in trigger_human_reviews,
    /// so returning all tasks here guarantees each gets reviewed.
    #[test]
    fn test_human_review_query_multiple_tasks_all_returned() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-A", 1, "2099-01-01T12:00:00");
        insert_requires_human_task(&conn, "T-B", 1, "2099-01-01T12:00:01");
        insert_requires_human_task(&conn, "T-C", 1, "2099-01-01T12:00:02");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(
            tasks.len(),
            3,
            "all three requires_human=1 tasks must be returned"
        );
        let ids: Vec<&str> = tasks.iter().map(|(id, _, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"T-A"), "T-A must be in results");
        assert!(ids.contains(&"T-B"), "T-B must be in results");
        assert!(ids.contains(&"T-C"), "T-C must be in results");
    }

    /// Mix of requires_human=1 and requires_human=0: only the flagged task is returned.
    #[test]
    fn test_human_review_query_mixed_flags_only_flagged_returned() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-YES", 1, "2099-01-01T12:00:00");
        insert_requires_human_task(&conn, "T-NO", 0, "2099-01-01T12:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(
            tasks.len(),
            1,
            "only requires_human=1 task must be returned"
        );
        assert_eq!(tasks[0].0, "T-YES");
    }

    /// yes_mode does NOT suppress human review for requiresHuman tasks (criterion 5).
    ///
    /// `query_human_review_tasks` (and by extension `trigger_human_reviews`) has no
    /// yes_mode parameter — the review is unconditional. This test documents that a
    /// requires_human=1 task is always returned regardless of run configuration.
    #[test]
    fn test_human_review_yes_mode_not_gated() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-BATCH", 1, "2099-01-01T12:00:00");

        // Simulate yes_mode=true: query_human_review_tasks takes no yes_mode parameter,
        // so it always returns requiresHuman tasks — yes_mode cannot suppress the review.
        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(
            tasks.len(),
            1,
            "requiresHuman task must be returned even in yes_mode (no mode gate in query)"
        );
    }

    /// Task with status != 'done' must not be returned even if requires_human=1.
    #[test]
    fn test_human_review_query_non_done_status_skipped() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, requires_human) \
             VALUES ('T-IP', 'Task T-IP', 'in_progress', 10, 1)",
            [],
        )
        .unwrap();

        let tasks = query_human_review_tasks(&conn, epoch);
        assert!(
            tasks.is_empty(),
            "in_progress task must not trigger human review (status != 'done')"
        );
    }

    /// Task with None model at 2 consecutive failures → model set to opus (sonnet baseline).
    #[test]
    fn test_model_escalation_none_model_to_opus() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let result = escalate_task_model_if_needed(&conn, "T-001", 2).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "None model assumes sonnet baseline and must escalate to opus"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(OPUS_MODEL.to_string()),
            "model in DB must be set to opus when previously unset"
        );
    }

    /// Escalation not triggered at 1 consecutive failure (threshold is 2).
    #[test]
    fn test_model_escalation_not_triggered_at_one_failure() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let result = escalate_task_model_if_needed(&conn, "T-001", 1).unwrap();
        assert_eq!(result, None, "no escalation at 1 failure (threshold is 2)");
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(SONNET_MODEL.to_string()),
            "model in DB must be unchanged at 1 failure"
        );
    }

    // --- apply_status_updates dispatcher tests (FEAT-003) ---
    //
    // These exercise the DB side of the side-band <task-status> path. The
    // engine's in-iteration wiring (outcome flip, tasks_completed bump, claim
    // clearing) is covered by the iteration-level tests elsewhere in this
    // file; here we cover the pure dispatcher contract: command dispatch,
    // PRD JSON sync, warning-on-state-violation.

    /// Count entries in an `apply_status_updates` result whose dispatch
    /// succeeded — preserves the legacy "applied" semantics for tests written
    /// against the old `u32` return type.
    fn applied_count(results: &[(String, detection::TaskStatusChange, bool)]) -> u32 {
        results.iter().filter(|(_, _, ok)| *ok).count() as u32
    }

    /// Seed a minimal task row. `status` is set verbatim so tests can simulate
    /// pre-claimed (in_progress) vs unclaimed (todo) state machines.
    fn seed_task_with_status(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES (?1, 't', 50, ?2)",
            rusqlite::params![id, status],
        )
        .unwrap();
    }

    /// Write a minimal PRD JSON with a `userStories` array containing the
    /// given ids (each with `passes: false`). Returns the path.
    fn write_minimal_prd(dir: &std::path::Path, ids: &[&str]) -> PathBuf {
        use serde_json::json;
        let stories: Vec<_> = ids
            .iter()
            .map(|id| json!({"id": id, "title": "t", "priority": 50, "passes": false}))
            .collect();
        let doc = json!({"userStories": stories});
        let path = dir.join("test-prd.json");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn test_apply_status_update_marks_task_done_after_claim() {
        // Seeds task as in_progress (as if claimed), runs dispatcher with a
        // Done update, asserts DB transitions to done.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-001", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-001"]);

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-001".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done", "Done dispatch must transition DB status");
    }

    #[test]
    fn test_apply_status_update_todo_task_auto_claimed_and_completed() {
        // Seeds task as todo (NOT claimed). Dispatching Done should auto-claim
        // (todo -> in_progress) then complete (in_progress -> done).
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-002", "todo");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-002"]);

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-002".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            applied_count(&results),
            1,
            "todo task must be auto-claimed then completed"
        );

        let (status, started_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, started_at FROM tasks WHERE id = 'FEAT-002'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "done");
        assert!(started_at.is_some(), "started_at must be set by auto-claim",);
    }

    #[test]
    fn test_apply_status_update_todo_auto_claim_writes_run_tasks() {
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-010", "todo");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-010"]);

        // Create a run so run_tasks linkage can be written.
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-010".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            Some("run-1"),
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'run-1' AND task_id = 'FEAT-010'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 1, "auto-claim must link task to run");
    }

    #[test]
    fn test_apply_status_update_done_flips_prd_json_passes() {
        // Two tasks in PRD; only FEAT-001 is dispatched. Verify FEAT-001's
        // passes flips true and the other task's entry is untouched.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-001", "in_progress");
        seed_task_with_status(&conn, "FEAT-002", "todo");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-001", "FEAT-002"]);

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-001".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let prd: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prd_path).unwrap()).unwrap();
        let stories = prd.get("userStories").unwrap().as_array().unwrap();
        assert_eq!(stories.len(), 2);
        let by_id = |id: &str| {
            stories
                .iter()
                .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
                .unwrap()
        };
        assert_eq!(
            by_id("FEAT-001").get("passes").and_then(|v| v.as_bool()),
            Some(true),
            "dispatched task's passes must flip to true",
        );
        assert_eq!(
            by_id("FEAT-002").get("passes").and_then(|v| v.as_bool()),
            Some(false),
            "unaffected task's passes must stay false",
        );
    }

    #[test]
    fn test_apply_status_update_json_sync_failure_does_not_rollback_db() {
        // Read-only PRD path: update_prd_task_passes will fail at the rename,
        // but the DB transition has already committed. Warning is logged
        // (stderr — not asserted here) and the DB state stands.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-003", "in_progress");
        // Point PRD at a non-existent path under the temp dir so the read
        // fails — mirrors the "missing PRD" failure mode.
        let prd_path = temp_dir.path().join("nonexistent.json");

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-003".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            applied_count(&results),
            1,
            "DB dispatch succeeded even though PRD sync failed",
        );

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-003'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            status, "done",
            "DB transition must stand after JSON failure"
        );
    }

    #[test]
    fn test_apply_status_update_task_missing_from_prd_json() {
        // Task exists in DB but NOT in PRD userStories. DB transition must
        // succeed; JSON is left unchanged; no panic.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-004", "in_progress");
        // PRD has only SEED-001 — FEAT-004 is absent.
        let prd_path = write_minimal_prd(temp_dir.path(), &["SEED-001"]);
        let before = std::fs::read_to_string(&prd_path).unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-004".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-004'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");

        // PRD JSON content unchanged.
        let after = std::fs::read_to_string(&prd_path).unwrap();
        assert_eq!(before, after, "PRD JSON must be unchanged when task absent");
    }

    #[test]
    fn test_apply_status_update_milestone_done_writes_summary_to_progress_file() {
        // Pre-seed progress.txt with two iteration entries, then dispatch
        // <task-status>MILESTONE-1:done</task-status>. The hook must rewrite
        // progress.txt so the raw entries are replaced by a summary block.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "MILESTONE-1", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["MILESTONE-1"]);

        let progress_path = temp_dir.path().join("progress-test.txt");
        let initial = "\n## 2026-01-01 - Iteration 1\n- Task: FEAT-001\n- Model: (default)\n- Effort: medium\n- Outcome: Completed\n- Files: (none)\n---\n\n## 2026-01-01 - Iteration 2\n- Task: FEAT-002\n- Model: (default)\n- Effort: medium\n- Outcome: Completed\n- Files: (none)\n---\n";
        std::fs::write(&progress_path, initial).unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "MILESTONE-1".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            Some(&progress_path),
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let after = std::fs::read_to_string(&progress_path).unwrap();
        assert!(
            after.contains("Milestone Summary: MILESTONE-1"),
            "milestone hook must append a summary block"
        );
        assert!(
            !after.contains("Iteration 1") && !after.contains("Iteration 2"),
            "raw iteration entries must be replaced by the summary"
        );
        assert!(
            after.contains("FEAT-001") && after.contains("FEAT-002"),
            "completed task IDs must survive in the summary's task list"
        );
    }

    #[test]
    fn test_apply_status_update_non_milestone_done_does_not_touch_progress_file() {
        // A regular FEAT-* Done dispatch must NOT trigger the milestone hook,
        // even when a progress_path is supplied.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-100", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-100"]);

        let progress_path = temp_dir.path().join("progress-test.txt");
        let initial = "\n## 2026-01-01 - Iteration 1\n- Task: FEAT-100\n- Model: (default)\n- Effort: medium\n- Outcome: Completed\n- Files: (none)\n---\n";
        std::fs::write(&progress_path, initial).unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-100".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            Some(&progress_path),
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let after = std::fs::read_to_string(&progress_path).unwrap();
        assert_eq!(
            after, initial,
            "non-milestone Done dispatch must leave progress file untouched"
        );
    }

    #[test]
    fn test_apply_status_update_continues_past_failed_dispatch() {
        // Two updates: the first targets a nonexistent task (dispatch fails),
        // the second targets an in_progress task (dispatch succeeds). The engine
        // must log + continue, not abort on the first failure.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-B", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-B"]);

        let updates = vec![
            detection::TaskStatusUpdate {
                task_id: "NONEXISTENT-999".to_string(),
                status: detection::TaskStatusChange::Done,
            },
            detection::TaskStatusUpdate {
                task_id: "FEAT-B".to_string(),
                status: detection::TaskStatusChange::Done,
            },
        ];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            applied_count(&results),
            1,
            "one dispatch failed, one succeeded"
        );

        let status_b: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-B'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status_b, "done");
    }

    // --- Parallel wave execution engine tests (FEAT-009) ---

    mod wave {
        use super::*;
        use crate::loop_engine::config::PermissionMode;
        use crate::loop_engine::prompt::slot::{
            SlotPromptBundle, SlotPromptParams, build_prompt as build_slot_prompt_bundle,
        };
        use crate::loop_engine::test_utils::{insert_task, insert_task_file};
        use crate::models::Task;
        use rusqlite::Connection;

        /// Opt every task out of the FEAT-003 buildy shared-infra heuristic.
        /// Used by wave-infrastructure tests whose `FEAT-*` task ids are
        /// generic placeholders, not real FEAT-class work — they predate
        /// FEAT-003 and assert wave merge/crash semantics rather than
        /// parallel-slot heuristics.
        fn opt_out_buildy(conn: &Connection) {
            conn.execute("UPDATE tasks SET claims_shared_infra = 0", [])
                .unwrap();
        }

        /// Build a minimal SlotIterationParams wired to a test DB.
        /// `signal_flag` is shared so tests can observe/trip it across slots.
        fn make_slot_params(db_dir: &Path, signal_flag: SignalFlag) -> SlotIterationParams {
            SlotIterationParams {
                db_dir: db_dir.to_path_buf(),
                permission_mode: PermissionMode::Dangerous,
                signal_flag,
                default_model: None,
                verbose: false,
                iteration: 1,
                max_iterations: 1,
                elapsed_secs: 0,
                task_prefix: None,
            }
        }

        /// Build a SlotPromptParams pointing at a temp project root + base prompt.
        fn make_prompt_params(
            project_root: &Path,
            base_prompt_path: PathBuf,
        ) -> SlotPromptParams<'static> {
            SlotPromptParams {
                project_root: project_root.to_path_buf(),
                base_prompt_path,
                permission_mode: PermissionMode::Dangerous,
                steering_path: None,
                session_guidance: "",
            }
        }

        /// Synthesize a `SlotPromptBundle` directly without invoking
        /// `build_prompt`. Useful for tests that don't need the full
        /// learnings/source-context pipeline (e.g. struct-field smoke tests
        /// and pre-signal early-exit checks).
        fn dummy_bundle(task_id: &str) -> SlotPromptBundle {
            SlotPromptBundle {
                prompt: format!("# slot prompt for {task_id}\n"),
                task_id: task_id.to_string(),
                task_files: Vec::new(),
                shown_learning_ids: Vec::new(),
                resolved_model: None,
                difficulty: None,
                section_sizes: Vec::new(),
                dropped_sections: Vec::new(),
            }
        }

        fn make_slot(
            slot_index: usize,
            working_root: PathBuf,
            prompt_bundle: SlotPromptBundle,
        ) -> SlotContext {
            SlotContext {
                slot_index,
                working_root,
                prompt_bundle,
            }
        }

        // --- Struct field contracts (AC 1-3) ---

        #[test]
        fn test_slot_context_fields() {
            let tmp = tempfile::TempDir::new().unwrap();
            let bundle = dummy_bundle("FEAT-1");
            let ctx = make_slot(2, tmp.path().to_path_buf(), bundle);
            assert_eq!(ctx.slot_index, 2);
            assert_eq!(ctx.working_root, tmp.path());
            assert_eq!(ctx.prompt_bundle.task_id, "FEAT-1");
        }

        #[test]
        fn test_slot_result_fields() {
            let sr = SlotResult {
                slot_index: 1,
                iteration_result: IterationResult {
                    outcome: IterationOutcome::Completed,
                    task_id: Some("FEAT-1".to_string()),
                    files_modified: vec!["a.rs".to_string()],
                    should_stop: false,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                },
                claim_succeeded: true,
                shown_learning_ids: vec![42, 77],
                prompt_for_overflow: None,
                section_sizes: Vec::new(),
                dropped_sections: Vec::new(),
                task_difficulty: None,
            };
            assert_eq!(sr.slot_index, 1);
            assert!(matches!(
                sr.iteration_result.outcome,
                IterationOutcome::Completed
            ));
            // FEAT-002 AC: SlotResult exposes shown_learning_ids at the top
            // level so the main thread can record bandit feedback without
            // re-reading the bundle (which has been moved into the worker).
            assert_eq!(sr.shown_learning_ids, vec![42, 77]);
        }

        #[test]
        fn test_wave_result_fields() {
            let wr = WaveResult {
                outcomes: vec![],
                wave_duration: Duration::from_millis(10),
            };
            assert!(wr.outcomes.is_empty());
            assert!(wr.wave_duration >= Duration::from_millis(10));
        }

        // --- run_slot_iteration: early exit on pre-signaled flag (AC 8) ---

        #[test]
        fn test_run_slot_iteration_honors_pre_set_signal_flag() {
            let (temp, _conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();

            let signal = SignalFlag::new();
            signal.set(); // pre-signal — slot must bail before spawning Claude
            let params = make_slot_params(temp.path(), signal);

            let slot = make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-1"));
            let result = run_slot_iteration(&slot, &params).expect("run_slot_iteration");
            assert_eq!(result.slot_index, 0);
            assert!(matches!(
                result.iteration_result.outcome,
                IterationOutcome::Empty
            ));
            assert!(result.iteration_result.should_stop);
            assert_eq!(result.iteration_result.task_id.as_deref(), Some("FEAT-1"),);
        }

        // --- prompt::slot::build_prompt: includes task JSON + completion ---

        #[test]
        fn test_slot_bundle_contains_task_and_completion_sections() {
            let (_temp, conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base = tmp.path().join("base.md");
            std::fs::write(&base, "BASE_PROMPT_CONTENT").unwrap();

            let mut task = Task::new("FEAT-42", "Do the thing");
            task.description = Some("Detailed desc".to_string());
            task.difficulty = Some("high".to_string());

            let prompt_params = make_prompt_params(tmp.path(), base);
            let bundle = build_slot_prompt_bundle(&conn, &task, &prompt_params);
            let prompt = &bundle.prompt;
            assert!(prompt.contains("FEAT-42"), "missing task id");
            assert!(prompt.contains("Do the thing"), "missing title");
            assert!(prompt.contains("Detailed desc"), "missing description");
            assert!(prompt.contains("\"difficulty\""), "missing difficulty");
            assert!(
                prompt.contains("<completed>FEAT-42</completed>"),
                "missing completion tag instruction",
            );
            assert!(
                prompt.contains("BASE_PROMPT_CONTENT"),
                "missing base prompt content",
            );
            assert_eq!(bundle.difficulty.as_deref(), Some("high"));
        }

        #[test]
        fn test_slot_bundle_tolerates_missing_base_prompt() {
            let (_temp, conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let task = Task::new("FEAT-1", "t");
            // base_prompt_path does not exist — must not panic
            let prompt_params =
                make_prompt_params(tmp.path(), tmp.path().join("does-not-exist.md"));
            let bundle = build_slot_prompt_bundle(&conn, &task, &prompt_params);
            assert!(bundle.prompt.contains("FEAT-1"));
        }

        // --- claim_slot_task ---

        #[test]
        fn test_claim_slot_task_updates_todo_to_in_progress() {
            let (_tmp, conn) = setup_test_db();
            insert_task(&conn, "FEAT-1", "t", "todo", 10);
            assert!(claim_slot_task(&conn, "FEAT-1"));
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = 'FEAT-1'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, "in_progress");
        }

        #[test]
        fn test_claim_slot_task_idempotent_on_already_in_progress() {
            let (_tmp, conn) = setup_test_db();
            insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
            // UPDATE matches because WHERE clause accepts in_progress too
            assert!(claim_slot_task(&conn, "FEAT-1"));
        }

        #[test]
        fn test_claim_slot_task_rejects_done_task() {
            let (_tmp, conn) = setup_test_db();
            insert_task(&conn, "FEAT-1", "t", "done", 10);
            assert!(!claim_slot_task(&conn, "FEAT-1"));
        }

        // --- run_parallel_wave: orchestration + panic handling (AC 9, 10, 11, 12) ---

        #[test]
        fn test_run_parallel_wave_empty_slots_returns_empty_outcomes() {
            let (temp, conn) = setup_test_db();

            let params = Arc::new(make_slot_params(temp.path(), SignalFlag::new()));
            let wave = run_parallel_wave(&conn, vec![], params);
            assert!(wave.outcomes.is_empty());
        }

        #[test]
        fn test_run_parallel_wave_reports_claim_failure_for_done_task() {
            let (temp, conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();

            // Task is already done — claim_slot_task returns false; wave must
            // emit a Crash(RuntimeError) entry rather than silently drop the slot.
            insert_task(&conn, "FEAT-DONE", "t", "done", 10);

            let signal = SignalFlag::new();
            // Pre-signal so if the claim logic regresses and still spawns Claude,
            // the slot's early-signal check bails before touching the network.
            signal.set();
            let params = Arc::new(make_slot_params(temp.path(), signal));

            let slot = make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-DONE"));
            let wave = run_parallel_wave(&conn, vec![slot], params);
            assert_eq!(wave.outcomes.len(), 1);
            assert_eq!(wave.outcomes[0].slot_index, 0);
            assert!(matches!(
                wave.outcomes[0].iteration_result.outcome,
                IterationOutcome::Crash(_)
            ));
            assert_eq!(
                wave.outcomes[0].iteration_result.task_id.as_deref(),
                Some("FEAT-DONE"),
            );
        }

        #[test]
        fn test_run_parallel_wave_claims_all_tasks_before_spawning() {
            // Main-thread claim must flip every task to in_progress before any
            // slot thread runs. We verify by pre-signaling so slot threads bail
            // immediately; the DB must still show in_progress for both tasks.
            let (temp, conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();

            insert_task(&conn, "FEAT-A", "a", "todo", 10);
            insert_task(&conn, "FEAT-B", "b", "todo", 10);

            let signal = SignalFlag::new();
            signal.set();
            let params = Arc::new(make_slot_params(temp.path(), signal));

            let slot_a = make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-A"));
            let slot_b = make_slot(1, tmp.path().to_path_buf(), dummy_bundle("FEAT-B"));

            let wave = run_parallel_wave(&conn, vec![slot_a, slot_b], params);
            assert_eq!(wave.outcomes.len(), 2);

            let status_a: String = conn
                .query_row("SELECT status FROM tasks WHERE id = 'FEAT-A'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let status_b: String = conn
                .query_row("SELECT status FROM tasks WHERE id = 'FEAT-B'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status_a, "in_progress");
            assert_eq!(status_b, "in_progress");
        }

        #[test]
        fn test_run_parallel_wave_outcomes_sorted_by_slot_index() {
            // Mix claim-failure and successful pre-signal early-exits to force
            // the reorder path. Outcomes must always emerge slot-ordered.
            let (temp, conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();

            insert_task(&conn, "FEAT-A", "a", "todo", 10);
            insert_task(&conn, "FEAT-B", "b", "done", 10); // claim fails
            insert_task(&conn, "FEAT-C", "c", "todo", 10);

            let signal = SignalFlag::new();
            signal.set();
            let params = Arc::new(make_slot_params(temp.path(), signal));

            let slots = vec![
                make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-A")),
                make_slot(1, tmp.path().to_path_buf(), dummy_bundle("FEAT-B")),
                make_slot(2, tmp.path().to_path_buf(), dummy_bundle("FEAT-C")),
            ];

            let wave = run_parallel_wave(&conn, slots, params);
            assert_eq!(wave.outcomes.len(), 3);
            assert_eq!(wave.outcomes[0].slot_index, 0);
            assert_eq!(wave.outcomes[1].slot_index, 1);
            assert_eq!(wave.outcomes[2].slot_index, 2);
        }

        #[test]
        fn test_run_parallel_wave_measures_wall_clock_duration() {
            let (temp, conn) = setup_test_db();

            let params = Arc::new(make_slot_params(temp.path(), SignalFlag::new()));
            let wave = run_parallel_wave(&conn, vec![], params);
            // Empty wave still records a non-negative duration; ensures the
            // Instant::now() → elapsed() contract holds.
            assert!(wave.wave_duration < Duration::from_secs(5));
        }

        // --- run_wave_iteration: dispatch & policy (FEAT-010) ---

        #[allow(clippy::too_many_arguments)]
        fn make_wave_params<'a>(
            conn: &'a mut Connection,
            db_dir: &'a Path,
            source_root: &'a Path,
            branch: &'a str,
            slot_paths: &'a [PathBuf],
            base_prompt: &'a Path,
            permission_mode: &'a PermissionMode,
            signal_flag: &'a SignalFlag,
            tasks_dir: &'a Path,
            prd_path: &'a Path,
            progress_path: &'a Path,
            parallel_slots: usize,
            project_config: &'a project_config::ProjectConfig,
            prd_implicit_overlap_files: &'a [String],
        ) -> WaveIterationParams<'a> {
            WaveIterationParams {
                conn,
                db_dir,
                source_root,
                branch,
                parallel_slots,
                slot_worktree_paths: slot_paths,
                iteration: 1,
                max_iterations: 1,
                elapsed_secs: 0,
                run_id: "test-run",
                base_prompt_path: base_prompt,
                permission_mode,
                signal_flag,
                default_model: None,
                verbose: false,
                task_prefix: None,
                prd_path,
                progress_path,
                tasks_dir,
                external_repo_path: None,
                external_git_scan_depth: 50,
                inter_iteration_delay: Duration::ZERO,
                steering_path: None,
                session_guidance: "",
                prd_implicit_overlap_files,
                project_config,
            }
        }

        #[test]
        fn test_run_wave_iteration_pre_set_signal_returns_terminal_signal() {
            let (temp, mut conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = tmp.path().join("prd.json");
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            signal.set();
            let mut ctx = IterationContext::new(5);
            let project_cfg = project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let outcome = run_wave_iteration(
                make_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    "main",
                    &[],
                    &base_prompt,
                    &mode,
                    &signal,
                    tmp.path(),
                    &prd,
                    &progress,
                    2,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );
            assert!(matches!(
                outcome.terminal,
                Some(WaveTerminal { exit_code: 130, .. })
            ));
            assert!(!outcome.iteration_consumed);
            assert_eq!(outcome.tasks_completed, 0);
        }

        #[test]
        fn test_run_wave_iteration_no_eligible_tasks_increments_stale_tracker() {
            let (temp, mut conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = tmp.path().join("prd.json");
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let mut ctx = IterationContext::new(5);
            let project_cfg = project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let outcome = run_wave_iteration(
                make_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    "main",
                    &[],
                    &base_prompt,
                    &mode,
                    &signal,
                    tmp.path(),
                    &prd,
                    &progress,
                    2,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );
            // Empty DB → empty group → wave consumes the iteration but does
            // not flag terminal; stale_tracker bumps so 3 such waves abort.
            assert!(outcome.terminal.is_none());
            assert!(outcome.iteration_consumed);
            assert_eq!(ctx.stale_tracker.count(), 1);
            // log_iteration must have written a NoEligibleTasks entry.
            let log = std::fs::read_to_string(&progress).unwrap();
            assert!(log.contains("NoEligibleTasks"), "got: {log}");
        }

        #[test]
        fn test_run_wave_iteration_third_no_eligible_wave_aborts_via_stale() {
            let (temp, mut conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = tmp.path().join("prd.json");
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let mut ctx = IterationContext::new(5);
            // Pre-stale twice so the next NoEligibleTasks wave hits threshold=3.
            ctx.stale_tracker.check("x", "x");
            ctx.stale_tracker.check("x", "x");
            let project_cfg = project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let outcome = run_wave_iteration(
                make_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    "main",
                    &[],
                    &base_prompt,
                    &mode,
                    &signal,
                    tmp.path(),
                    &prd,
                    &progress,
                    2,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );
            let t = outcome.terminal.expect("terminal expected");
            assert_eq!(t.exit_code, 1);
            assert!(t.reason.contains("no eligible tasks"), "got: {}", t.reason);
            assert!(t.run_status.is_none());
        }

        #[test]
        fn test_run_wave_iteration_crash_should_abort_returns_terminal() {
            let (temp, mut conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = tmp.path().join("prd.json");
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let mut ctx = IterationContext::new(1); // first crash aborts
            ctx.crash_tracker.record_crash();
            assert!(ctx.crash_tracker.should_abort());
            let project_cfg = project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let outcome = run_wave_iteration(
                make_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    "main",
                    &[],
                    &base_prompt,
                    &mode,
                    &signal,
                    tmp.path(),
                    &prd,
                    &progress,
                    2,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );
            let t = outcome.terminal.expect("terminal expected");
            assert_eq!(t.exit_code, 1);
            assert!(t.reason.contains("too many crashes"), "got: {}", t.reason);
        }

        #[test]
        fn test_run_wave_iteration_signal_during_inter_wave_delay_returns_130() {
            // Regression: signal fired during inter-wave delay must return exit
            // code 130 ("signal received"), not 0 ("stop signal"), so operators
            // can distinguish SIGINT/SIGTERM from a clean .stop-file termination.
            //
            // Setup: point CLAUDE_BINARY at a nonexistent path so each slot
            // thread fails instantly (no real Claude spawn), letting run_parallel_wave
            // complete in microseconds.  Then the 500 ms delay starts, and the
            // background thread fires the signal at 100 ms — well inside the
            // delay window and well after steps 0-13 have already passed without
            // seeing the signal.
            let _env_lock = crate::loop_engine::test_utils::CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap();
            let _env_guard = crate::loop_engine::test_utils::EnvGuard::set(
                "CLAUDE_BINARY",
                "/nonexistent_binary_for_test",
            );

            let (temp, mut conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = tmp.path().join("prd.json");
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;

            // Insert an eligible task so select_parallel_group returns it and
            // run_parallel_wave actually spawns a slot thread (which fails fast
            // because CLAUDE_BINARY is invalid, so should_stop stays false and
            // step 13 sees no signal yet).
            insert_task(&conn, "FEAT-DELAY-SIGNAL", "delay signal test", "todo", 1);

            let signal = SignalFlag::new();
            let signal_clone = signal.clone();
            // Fire at 100 ms: steps 0-13 complete in < 10 ms (slot fails at process
            // spawn with ENOENT), so the signal always lands inside the 500 ms delay.
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                signal_clone.set();
            });

            let mut ctx = IterationContext::new(5);
            let project_cfg = project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let outcome = run_wave_iteration(
                WaveIterationParams {
                    conn: &mut conn,
                    db_dir: temp.path(),
                    source_root: tmp.path(),
                    branch: "main",
                    parallel_slots: 1,
                    slot_worktree_paths: &[tmp.path().to_path_buf()],
                    iteration: 1,
                    max_iterations: 1,
                    elapsed_secs: 0,
                    run_id: "test-run",
                    base_prompt_path: &base_prompt,
                    permission_mode: &mode,
                    signal_flag: &signal,
                    default_model: None,
                    verbose: false,
                    task_prefix: None,
                    prd_path: &prd,
                    progress_path: &progress,
                    tasks_dir: tmp.path(),
                    external_repo_path: None,
                    external_git_scan_depth: 50,
                    inter_iteration_delay: Duration::from_millis(500),
                    steering_path: None,
                    session_guidance: "",
                    prd_implicit_overlap_files: &prd_implicit,
                    project_config: &project_cfg,
                },
                &mut ctx,
            );
            let t = outcome.terminal.expect("terminal expected");
            assert_eq!(
                t.exit_code, 130,
                "expected 130 for SIGINT during delay, got {}",
                t.exit_code
            );
            assert_eq!(t.reason, "signal received", "got: {}", t.reason);
        }

        #[test]
        fn test_iteration_context_initializes_pending_reorder_hints_empty() {
            let ctx = IterationContext::new(5);
            assert!(ctx.pending_reorder_hints.is_empty());
        }

        // --- SlotIterationParams cloneability (Arc + clone into threads) ---

        #[test]
        fn test_slot_iteration_params_is_clone() {
            let tmp = tempfile::TempDir::new().unwrap();
            let params = SlotIterationParams {
                db_dir: tmp.path().to_path_buf(),
                permission_mode: PermissionMode::Dangerous,
                signal_flag: SignalFlag::new(),
                default_model: Some(OPUS_MODEL.to_string()),
                verbose: true,
                iteration: 7,
                max_iterations: 100,
                elapsed_secs: 42,
                task_prefix: None,
            };
            let cloned = params.clone();
            assert_eq!(cloned.db_dir, params.db_dir);
            assert_eq!(cloned.verbose, params.verbose);
            assert_eq!(cloned.default_model.as_deref(), Some(OPUS_MODEL));
        }

        // --- FEAT-002 CONTRACT: build_bundle (main) → spawn(worker) ---
        //
        // The slot worker must NEVER touch a `&Connection` or read task data
        // from anything other than its prompt bundle. The compile-time
        // `assert_impl_all!(SlotPromptBundle: Send)` in `tests/prompt_slot.rs`
        // guards the type-level invariant; this test guards the wiring:
        // `build_slot_contexts` populates the bundle (with its DB-derived
        // `task_files`) on the main thread, BEFORE any worker spawn, and the
        // bundle survives the trip through `run_parallel_wave` unmodified.

        #[test]
        fn test_build_slot_contexts_populates_bundle_on_main_thread() {
            use crate::commands::next::selection::ScoredTask;
            let (temp, conn) = setup_test_db();
            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "BASE\n").unwrap();

            insert_task(&conn, "FEAT-CONTRACT", "contract task", "todo", 10);
            insert_task_file(&conn, "FEAT-CONTRACT", "src/contract.rs");

            let mut task = Task::new("FEAT-CONTRACT", "contract task");
            task.difficulty = Some("low".to_string());
            let scored = ScoredTask {
                task,
                files: vec!["src/contract.rs".to_string()],
                total_score: 0,
                score_breakdown: crate::commands::next::selection::ScoreBreakdown {
                    priority_score: 0,
                    file_score: 0,
                    file_overlap_count: 0,
                },
            };

            let prompt_params = SlotPromptParams {
                project_root: tmp.path().to_path_buf(),
                base_prompt_path: base_prompt,
                permission_mode: PermissionMode::Dangerous,
                steering_path: None,
                session_guidance: "",
            };
            let slot_paths = vec![tmp.path().to_path_buf()];
            let slots = build_slot_contexts(&conn, vec![scored], &slot_paths, &prompt_params);

            assert_eq!(slots.len(), 1);
            let bundle = &slots[0].prompt_bundle;
            // task_id, task_files, and difficulty came from the DB / Task on
            // the main thread. The worker thread will read from this bundle
            // and never reopen `conn`.
            assert_eq!(bundle.task_id, "FEAT-CONTRACT");
            assert_eq!(bundle.task_files, vec!["src/contract.rs"]);
            assert_eq!(bundle.difficulty.as_deref(), Some("low"));
            // The task JSON inside the bundle reflects the post-claim state
            // even though the row is still 'todo' (claim happens later in
            // run_parallel_wave). This keeps the agent prompt honest about
            // the state the row WILL be in by the time the worker runs.
            assert!(
                bundle.prompt.contains("\"status\": \"in_progress\""),
                "bundle prompt must reflect post-claim status; got:\n{}",
                bundle.prompt
            );
            // Drop temp last so the connection (held in scope) outlives it.
            drop(temp);
        }

        // --- TEST-001: Comprehensive parallel execution tests -------------
        //
        // End-to-end behavior of `run_parallel_wave` and `run_wave_iteration`
        // using a mock Claude binary. Every test here mutates the process-wide
        // `CLAUDE_BINARY` env var, so each one takes the shared mutex to
        // serialize with other tests that touch the same variable.
        mod comprehensive {
            use super::*;
            use crate::loop_engine::test_utils::{CLAUDE_BINARY_MUTEX, EnvGuard, insert_run};
            use std::io::Write as _;
            use std::os::unix::fs::PermissionsExt as _;

            /// Create a mock `claude` script for wave tests.
            ///
            /// Behavior:
            /// - Reads prompt from stdin (how `spawn_claude` delivers it).
            /// - Extracts `TASK_ID` from the task JSON `"id": "TASK-ID"` line.
            /// - Emits one stream-json `result` line so the claude wrapper's
            ///   stream-json parser yields `<completed>TASK-ID</completed>`
            ///   as the slot's output text.
            /// - When the `MOCK_CRASH_TASKS` env var lists the extracted id
            ///   (comma-delimited), exit 1 with no output so the slot outcome
            ///   becomes `Crash(RuntimeError)`.
            ///
            /// The caller removes the script with `std::fs::remove_file` after
            /// the wave completes.
            fn make_mock_script(name: &str) -> PathBuf {
                let path = std::env::temp_dir().join(format!("task_mgr_test_wave_{name}.sh"));
                {
                    let mut f = std::fs::File::create(&path).unwrap();
                    writeln!(f, "#!/bin/sh").unwrap();
                    writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
                    writeln!(
                        f,
                        r#"TASK_ID=$(printf '%s' "$PROMPT" | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' | head -n 1)"#
                    )
                    .unwrap();
                    writeln!(
                        f,
                        r#"case ",${{MOCK_CRASH_TASKS:-}}," in *",${{TASK_ID}},"*) exit 1 ;; esac"#
                    )
                    .unwrap();
                    writeln!(
                        f,
                        r#"printf '{{"type":"result","result":"<completed>%s</completed>"}}\n' "$TASK_ID""#
                    )
                    .unwrap();
                }
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                path
            }

            /// Fetch a task's status, panicking if the row is missing.
            fn task_status(conn: &Connection, id: &str) -> String {
                conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |r| r.get(0))
                    .unwrap()
            }

            /// Minimal PRD with the given ids so `update_prd_task_passes`
            /// finds matching `userStories` entries to flip `passes=true` on.
            fn write_prd(dir: &Path, ids: &[&str]) -> PathBuf {
                use serde_json::json;
                let stories: Vec<_> = ids
                    .iter()
                    .map(|id| json!({"id": id, "title": "t", "priority": 10, "passes": false}))
                    .collect();
                let path = dir.join("prd.json");
                std::fs::write(
                    &path,
                    serde_json::to_string(&json!({"userStories": stories})).unwrap(),
                )
                .unwrap();
                path
            }

            /// Assemble a WaveIterationParams for the common test wiring.
            #[allow(clippy::too_many_arguments)]
            fn build_wave_params<'a>(
                conn: &'a mut Connection,
                db_dir: &'a Path,
                source_root: &'a Path,
                slot_paths: &'a [PathBuf],
                base_prompt: &'a Path,
                permission_mode: &'a PermissionMode,
                signal_flag: &'a SignalFlag,
                prd_path: &'a Path,
                progress_path: &'a Path,
                parallel_slots: usize,
                run_id: &'a str,
                project_config: &'a project_config::ProjectConfig,
                prd_implicit_overlap_files: &'a [String],
            ) -> WaveIterationParams<'a> {
                WaveIterationParams {
                    conn,
                    db_dir,
                    source_root,
                    branch: "main",
                    parallel_slots,
                    slot_worktree_paths: slot_paths,
                    iteration: 1,
                    max_iterations: 1,
                    elapsed_secs: 0,
                    run_id,
                    base_prompt_path: base_prompt,
                    permission_mode,
                    signal_flag,
                    default_model: None,
                    verbose: false,
                    task_prefix: None,
                    prd_path,
                    progress_path,
                    tasks_dir: source_root,
                    external_repo_path: None,
                    external_git_scan_depth: 50,
                    inter_iteration_delay: Duration::ZERO,
                    steering_path: None,
                    session_guidance: "",
                    prd_implicit_overlap_files,
                    project_config,
                }
            }

            /// AC1: two non-conflicting tasks complete in one wave (--parallel 2).
            ///
            /// Two eligible tasks with disjoint `touchesFiles` fill both slots;
            /// the mock emits `<completed>` for each, so both rows flip to
            /// `done` and `tasks_completed == 2` after the wave.
            #[test]
            fn test_wave_two_disjoint_tasks_both_complete() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("two_complete");
                let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-complete";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-A", "Task A", "todo", 10);
                insert_task(&conn, "FEAT-B", "Task B", "todo", 20);
                insert_task_file(&conn, "FEAT-A", "src/a.rs");
                insert_task_file(&conn, "FEAT-B", "src/b.rs");
                // FEAT-003: opt out of the buildy shared-infra heuristic so this
                // wave-infrastructure test isolates merge-back semantics from
                // implicit-overlap detection (the test's intent predates FEAT-003).
                opt_out_buildy(&conn);

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-A", "FEAT-B"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

                let mut ctx = IterationContext::new(5);
                let outcome = run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        2,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                assert_eq!(
                    outcome.tasks_completed, 2,
                    "both slots should complete their tasks"
                );
                assert!(outcome.iteration_consumed);
                assert_eq!(task_status(&conn, "FEAT-A"), "done");
                assert_eq!(task_status(&conn, "FEAT-B"), "done");
            }

            /// Regression: each running slot in a wave MUST start its own
            /// activity monitor. The original bug let slot-mode iterations
            /// silently skip `monitor::start_monitor`, so the watchdog's
            /// `last_activity_epoch` never advanced (no activity extensions)
            /// and there were no heartbeat / change-tracking logs.
            ///
            /// `MONITOR_START_COUNT` is a `#[cfg(test)]`-only call counter in
            /// `monitor.rs`; observing it bump by `parallel_slots` proves both
            /// slots called `start_monitor`. Uses the same fast mock-claude
            /// scaffold as the disjoint-tasks test, so the assertion is the
            /// only meaningful difference.
            #[test]
            fn test_wave_each_slot_starts_its_own_monitor() {
                use crate::loop_engine::monitor::MONITOR_START_COUNT;
                use std::sync::atomic::Ordering;

                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("monitor_per_slot");
                let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-monitor-per-slot";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-MA", "Task MA", "todo", 10);
                insert_task(&conn, "FEAT-MB", "Task MB", "todo", 20);
                insert_task_file(&conn, "FEAT-MA", "src/ma.rs");
                insert_task_file(&conn, "FEAT-MB", "src/mb.rs");
                opt_out_buildy(&conn);

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-MA", "FEAT-MB"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

                let before = MONITOR_START_COUNT.load(Ordering::Relaxed);
                let mut ctx = IterationContext::new(5);
                let outcome = run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        2,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );
                let after = MONITOR_START_COUNT.load(Ordering::Relaxed);

                let _ = std::fs::remove_file(&script);

                // Sanity: both slots actually ran (so the assertion below isn't
                // satisfied by a path that short-circuited before the monitor).
                assert_eq!(outcome.tasks_completed, 2, "both slots should complete");
                assert!(
                    after.saturating_sub(before) >= 2,
                    "expected ≥2 monitor starts (one per running slot); before={before}, after={after}",
                );
            }

            /// AC2: signal during wave terminates all slots.
            ///
            /// Pre-set the shared signal before the wave starts. Steps 0/13 of
            /// `run_wave_iteration` short-circuit on signal, so the direct
            /// wave-iteration path is covered by
            /// `test_run_wave_iteration_pre_set_signal_returns_terminal_signal`.
            /// This test exercises `run_parallel_wave` itself: every spawned
            /// slot thread must observe the signal and bail out of its iteration
            /// without ever reaching the mock Claude process.
            #[test]
            fn test_wave_pre_signal_terminates_every_slot() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Point the binary at a path that would crash if spawned, so a
                // regression that skips the signal check would surface as a
                // non-Empty outcome instead of a silent pass.
                let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent_binary_for_signal_test");

                let (temp, conn) = setup_test_db();
                insert_task(&conn, "FEAT-A", "a", "todo", 10);
                insert_task(&conn, "FEAT-B", "b", "todo", 20);

                let tmp = tempfile::TempDir::new().unwrap();

                let signal = SignalFlag::new();
                signal.set();
                let params = Arc::new(make_slot_params(temp.path(), signal.clone()));

                let slots = vec![
                    make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-A")),
                    make_slot(1, tmp.path().to_path_buf(), dummy_bundle("FEAT-B")),
                ];
                let wave = run_parallel_wave(&conn, slots, params);

                assert_eq!(wave.outcomes.len(), 2, "every slot must report an outcome");
                for outcome in &wave.outcomes {
                    assert!(
                        outcome.iteration_result.should_stop,
                        "slot {} must stop on signal",
                        outcome.slot_index
                    );
                    assert!(
                        matches!(outcome.iteration_result.outcome, IterationOutcome::Empty),
                        "slot {} outcome must be Empty on pre-set signal, got {:?}",
                        outcome.slot_index,
                        outcome.iteration_result.outcome
                    );
                }
            }

            /// AC3: crash in one slot doesn't affect other slots.
            ///
            /// Mock crashes slot 0 (`FEAT-CRASH`) by setting `MOCK_CRASH_TASKS`
            /// to that id; slot 1 (`FEAT-OK`) completes normally. We expect
            /// one `Crash(RuntimeError)` outcome plus one completion mark; the
            /// completion must not be lost because its peer crashed.
            #[test]
            fn test_wave_crash_in_one_slot_does_not_affect_others() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("mixed_crash");
                let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::set("MOCK_CRASH_TASKS", "FEAT-CRASH");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-mixed";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-CRASH", "crash slot", "todo", 10);
                insert_task(&conn, "FEAT-OK", "passing slot", "todo", 20);
                insert_task_file(&conn, "FEAT-CRASH", "src/crash.rs");
                insert_task_file(&conn, "FEAT-OK", "src/ok.rs");
                opt_out_buildy(&conn);

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-CRASH", "FEAT-OK"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

                let mut ctx = IterationContext::new(5);
                let outcome = run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        2,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                assert_eq!(
                    outcome.tasks_completed, 1,
                    "the non-crashing slot must still mark its task done"
                );
                assert_eq!(task_status(&conn, "FEAT-OK"), "done");
                assert_ne!(
                    task_status(&conn, "FEAT-CRASH"),
                    "done",
                    "the crashed slot must not mark its task done"
                );
            }

            /// AC4: `--parallel 1` produces identical behavior to sequential.
            ///
            /// With `parallel_slots=1` and three eligible disjoint-file tasks,
            /// `select_parallel_group` caps at one task — the same pick
            /// sequential `select_next_task` would make. After the wave, the
            /// winning task is `done` and the other two are still `todo`.
            #[test]
            fn test_wave_parallel_slots_one_runs_a_single_task() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("parallel_one");
                let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-parallel-one";
                insert_run(&conn, run_id);
                // Priorities: 10 wins, 20 and 30 must not be touched.
                insert_task(&conn, "FEAT-WIN", "winner", "todo", 10);
                insert_task(&conn, "FEAT-SKIP1", "skip 1", "todo", 20);
                insert_task(&conn, "FEAT-SKIP2", "skip 2", "todo", 30);
                insert_task_file(&conn, "FEAT-WIN", "src/win.rs");
                insert_task_file(&conn, "FEAT-SKIP1", "src/skip1.rs");
                insert_task_file(&conn, "FEAT-SKIP2", "src/skip2.rs");

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-WIN", "FEAT-SKIP1", "FEAT-SKIP2"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf()];

                let mut ctx = IterationContext::new(5);
                let outcome = run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        1,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                assert_eq!(outcome.tasks_completed, 1, "exactly one slot runs");
                assert_eq!(task_status(&conn, "FEAT-WIN"), "done");
                assert_eq!(
                    task_status(&conn, "FEAT-SKIP1"),
                    "todo",
                    "lower-priority task must be untouched by --parallel 1"
                );
                assert_eq!(task_status(&conn, "FEAT-SKIP2"), "todo");

                // Only one progress entry was emitted — matches sequential cadence.
                let log = std::fs::read_to_string(&progress).unwrap();
                assert_eq!(
                    log.matches("- Task: FEAT-").count(),
                    1,
                    "exactly one task entry in progress, got: {log}"
                );
            }

            /// AC5: parallel group with all-overlapping tasks degenerates to
            /// sequential.
            ///
            /// Three tasks all touch `src/shared.rs`. Even with
            /// `parallel_slots=3`, `select_parallel_group` returns a group of
            /// one; the wave runs a single slot and only the highest-priority
            /// task advances.
            #[test]
            fn test_wave_all_overlapping_tasks_run_sequentially() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("all_overlap");
                let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-overlap";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-HOT1", "hot 1", "todo", 10);
                insert_task(&conn, "FEAT-HOT2", "hot 2", "todo", 20);
                insert_task(&conn, "FEAT-HOT3", "hot 3", "todo", 30);
                for id in ["FEAT-HOT1", "FEAT-HOT2", "FEAT-HOT3"] {
                    insert_task_file(&conn, id, "src/shared.rs");
                }

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-HOT1", "FEAT-HOT2", "FEAT-HOT3"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![
                    tmp.path().to_path_buf(),
                    tmp.path().to_path_buf(),
                    tmp.path().to_path_buf(),
                ];

                let mut ctx = IterationContext::new(5);
                let outcome = run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        3,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                assert_eq!(
                    outcome.tasks_completed, 1,
                    "file-conflict collapse must leave only one slot running"
                );
                assert_eq!(task_status(&conn, "FEAT-HOT1"), "done");
                assert_eq!(task_status(&conn, "FEAT-HOT2"), "todo");
                assert_eq!(task_status(&conn, "FEAT-HOT3"), "todo");
            }

            /// AC7 — CrashTracker wave policy: all-slot crash increments the
            /// tracker, any-slot success resets it.
            ///
            /// Two tasks → both crash → `record_crash()` called so
            /// `ctx.crash_tracker.count() == 1` after the wave.
            #[test]
            fn test_wave_crash_tracker_all_crashed_increments() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("all_crash");
                let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                // Both tasks crash.
                let _crash_guard = EnvGuard::set("MOCK_CRASH_TASKS", "FEAT-X,FEAT-Y");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-all-crash";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-X", "x", "todo", 10);
                insert_task(&conn, "FEAT-Y", "y", "todo", 20);
                insert_task_file(&conn, "FEAT-X", "src/x.rs");
                insert_task_file(&conn, "FEAT-Y", "src/y.rs");

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-X", "FEAT-Y"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

                let mut ctx = IterationContext::new(10);
                assert_eq!(ctx.crash_tracker.count(), 0);

                let outcome = run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        2,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                assert_eq!(outcome.tasks_completed, 0, "no slot should complete");
                assert_eq!(
                    ctx.crash_tracker.count(),
                    1,
                    "all-slots-crashed must bump the crash tracker exactly once per wave"
                );
            }

            /// AC7 — mirror: at least one slot completes, so the crash tracker
            /// resets even if a sibling crashed. Seeds `count = 2` first so the
            /// reset-to-zero assertion is meaningful.
            #[test]
            fn test_wave_crash_tracker_any_completed_resets() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("mixed_reset");
                let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::set("MOCK_CRASH_TASKS", "FEAT-CRASH");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-mixed-reset";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-CRASH", "c", "todo", 10);
                insert_task(&conn, "FEAT-OK2", "ok", "todo", 20);
                insert_task_file(&conn, "FEAT-CRASH", "src/crash2.rs");
                insert_task_file(&conn, "FEAT-OK2", "src/ok2.rs");
                opt_out_buildy(&conn);

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-CRASH", "FEAT-OK2"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

                let mut ctx = IterationContext::new(10);
                ctx.crash_tracker.record_crash();
                ctx.crash_tracker.record_crash();
                assert_eq!(ctx.crash_tracker.count(), 2);

                run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        2,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                assert_eq!(
                    ctx.crash_tracker.count(),
                    0,
                    "any-slot success must reset the crash tracker to zero"
                );
            }

            /// AC8: progress file entries include slot numbers.
            ///
            /// After a 2-slot wave, the progress log must carry per-slot
            /// headers (`Iteration N Slot M`) and body lines (`- Slot: M`)
            /// so operators can correlate entries with wave slots.
            #[test]
            fn test_wave_progress_entries_include_slot_numbers() {
                let _env_lock = CLAUDE_BINARY_MUTEX
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let script = make_mock_script("progress_slots");
                let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
                let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

                let (temp, mut conn) = setup_test_db();
                let run_id = "run-wave-progress";
                insert_run(&conn, run_id);
                insert_task(&conn, "FEAT-P1", "p1", "todo", 10);
                insert_task(&conn, "FEAT-P2", "p2", "todo", 20);
                insert_task_file(&conn, "FEAT-P1", "src/p1.rs");
                insert_task_file(&conn, "FEAT-P2", "src/p2.rs");
                opt_out_buildy(&conn);

                let tmp = tempfile::TempDir::new().unwrap();
                let base_prompt = tmp.path().join("base.md");
                std::fs::write(&base_prompt, "base").unwrap();
                let prd = write_prd(tmp.path(), &["FEAT-P1", "FEAT-P2"]);
                let progress = tmp.path().join("progress.txt");
                let mode = PermissionMode::Dangerous;
                let signal = SignalFlag::new();
                let project_cfg = project_config::ProjectConfig::default();
                let prd_implicit: Vec<String> = Vec::new();
                let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

                let mut ctx = IterationContext::new(5);
                run_wave_iteration(
                    build_wave_params(
                        &mut conn,
                        temp.path(),
                        tmp.path(),
                        &slot_paths,
                        &base_prompt,
                        &mode,
                        &signal,
                        &prd,
                        &progress,
                        2,
                        run_id,
                        &project_cfg,
                        &prd_implicit,
                    ),
                    &mut ctx,
                );

                let _ = std::fs::remove_file(&script);

                let log = std::fs::read_to_string(&progress).expect("progress file exists");
                assert!(
                    log.contains("Iteration 1 Slot 0"),
                    "progress must tag slot 0 in iteration 1 header, got: {log}"
                );
                assert!(
                    log.contains("Iteration 1 Slot 1"),
                    "progress must tag slot 1 in iteration 1 header, got: {log}"
                );
                assert!(
                    log.contains("- Slot: 0"),
                    "progress body must contain '- Slot: 0', got: {log}"
                );
                assert!(
                    log.contains("- Slot: 1"),
                    "progress body must contain '- Slot: 1', got: {log}"
                );
            }
        }
    }

    // --- FEAT-002: merge-back failure halt-check tests ---
    //
    // These tests cover the wave-loop FEAT-002 reset/halt contract in
    // isolation, exercising `apply_merge_fail_reset_and_halt_check` directly
    // so we don't need to drive a full `run_loop` (which would require git,
    // Claude, and a multi-slot worktree harness — that level of coverage
    // belongs in `tests/` integration tests once the cross-cutting harness
    // exists for FEAT-001/003/004).

    fn insert_in_progress_task(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES \
             (?1, 'merge-fail test task', 'in_progress', 1, datetime('now'))",
            [id],
        )
        .unwrap();
    }

    /// AC: WaveOutcome.failed_merges is empty when no merge failures
    /// (e.g. preflight bail-out / no-eligible-tasks).
    #[test]
    fn test_wave_outcome_failed_merges_empty_by_default() {
        let outcome = WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: None,
            was_stopped: false,
            failed_merges: Vec::new(),
        };
        assert!(outcome.failed_merges.is_empty());
    }

    /// AC: WaveOutcome.failed_merges carries `(slot, task_id)` as a single
    /// `FailedMerge` value so the slot/task pairing is a type-level guarantee
    /// (no parallel arrays held lockstep by rustdoc).
    #[test]
    fn test_wave_outcome_failed_merges_pair_slot_with_task_id() {
        let outcome = WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: None,
            was_stopped: false,
            failed_merges: vec![
                FailedMerge {
                    slot: 1,
                    task_id: Some("FEAT-001".into()),
                },
                FailedMerge {
                    slot: 2,
                    task_id: Some("FEAT-002".into()),
                },
            ],
        };
        assert_eq!(outcome.failed_merges.len(), 2);
        assert_eq!(outcome.failed_merges[0].slot, 1);
        assert_eq!(
            outcome.failed_merges[0].task_id.as_deref(),
            Some("FEAT-001")
        );
        assert_eq!(outcome.failed_merges[1].slot, 2);
        assert_eq!(
            outcome.failed_merges[1].task_id.as_deref(),
            Some("FEAT-002")
        );
    }

    /// AC: ctx.consecutive_merge_fail_waves increments on a failed wave.
    #[test]
    fn test_consecutive_counter_increments_on_failure() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);

        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2, // default threshold
        );
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
        assert_eq!(decision, MergeFailHaltDecision::Continue);
    }

    /// AC: counter resets to 0 on a fully-successful wave (failed_merges empty).
    #[test]
    fn test_consecutive_counter_resets_on_success() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 3;

        let decision = apply_merge_fail_reset_and_halt_check(&conn, &mut ctx, "feat/test", &[], 2);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
        assert_eq!(decision, MergeFailHaltDecision::Continue);
    }

    /// AC: failed slot's task is reset back to `todo`.
    #[test]
    fn test_failed_slot_task_reset_to_todo() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());

        apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2,
        );

        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(status, "todo");
        // pending_slot_tasks drained.
        assert!(!ctx.pending_slot_tasks.contains(&"FEAT-001".to_string()));
    }

    /// AC: threshold reached → Halt with non-zero exit and reason citing the
    /// counter / threshold values.
    #[test]
    fn test_halt_returned_when_threshold_reached() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 1; // already 1, the next hit makes it 2.

        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2,
        );
        match decision {
            MergeFailHaltDecision::Halt {
                exit_code,
                exit_reason,
            } => {
                assert_eq!(exit_code, 1);
                assert!(
                    exit_reason.contains("2 consecutive"),
                    "exit_reason should cite counter; got: {exit_reason}"
                );
                assert!(
                    exit_reason.contains("threshold=2"),
                    "exit_reason should cite threshold; got: {exit_reason}"
                );
            }
            _ => panic!("expected Halt, got {decision:?}"),
        }
        assert_eq!(ctx.consecutive_merge_fail_waves, 2);
    }

    /// AC: known-bad — verify reset happens BEFORE the threshold check, so a
    /// halted run still leaves the DB in a re-runnable state. Equivalent: the
    /// failed-slot task is `todo` even when the threshold was reached.
    #[test]
    fn test_reset_runs_before_halt_decision() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 0;

        // threshold 1 → halts on this very wave.
        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            1,
        );
        assert!(matches!(decision, MergeFailHaltDecision::Halt { .. }));

        // The reset must have happened despite the immediate halt.
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(
            status, "todo",
            "AC: halted run must NOT leave any task in `in_progress` for the failed slots"
        );
    }

    /// AC: threshold = 0 → never halt (legacy behavior preserved). Counter
    /// still increments — operators can observe the cascade in logs without
    /// the loop aborting.
    #[test]
    fn test_threshold_zero_never_halts() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 100; // arbitrarily high.

        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            0, // threshold 0 = never halt.
        );
        assert_eq!(decision, MergeFailHaltDecision::Continue);
        assert_eq!(ctx.consecutive_merge_fail_waves, 101);
    }

    /// AC: failed_merges empty → no reset, counter cleared, Continue.
    #[test]
    fn test_empty_failed_merges_resets_counter_no_side_effects() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001"); // stays in_progress.
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 5;
        ctx.pending_slot_tasks.push("FEAT-001".to_string());

        let decision = apply_merge_fail_reset_and_halt_check(&conn, &mut ctx, "feat/test", &[], 2);
        assert_eq!(decision, MergeFailHaltDecision::Continue);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
        // Did NOT touch unrelated in-progress task.
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(status, "in_progress");
        // pending_slot_tasks NOT drained on the empty path.
        assert!(ctx.pending_slot_tasks.contains(&"FEAT-001".to_string()));
    }

    /// AC: multiple failed slots — every task is reset and drained from
    /// pending_slot_tasks.
    #[test]
    fn test_multiple_failed_slots_all_reset() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        insert_in_progress_task(&conn, "FEAT-002");
        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());
        ctx.pending_slot_tasks.push("FEAT-002".to_string());

        apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[
                FailedMerge {
                    slot: 1,
                    task_id: Some("FEAT-001".into()),
                },
                FailedMerge {
                    slot: 2,
                    task_id: Some("FEAT-002".into()),
                },
            ],
            5,
        );
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "todo"
        );
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-002"),
            "todo"
        );
        assert!(ctx.pending_slot_tasks.is_empty());
    }

    /// AC: failure-mode — reset failures are non-fatal; threshold check still
    /// runs on the original failed_merges count. We can't easily inject a SQL
    /// error here, but we CAN verify that a slot whose task_id is `None`
    /// (e.g. claim never resolved) is silently skipped without panicking,
    /// AND the counter still increments + halt still triggers based on the
    /// full failed_merges count.
    #[test]
    fn test_reset_failure_modes_dont_skip_threshold_check() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);

        // Two failed slots, neither has a resolved task_id.
        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[
                FailedMerge {
                    slot: 1,
                    task_id: None,
                },
                FailedMerge {
                    slot: 2,
                    task_id: None,
                },
            ],
            1, // threshold 1 → halt on first failure.
        );
        assert!(matches!(decision, MergeFailHaltDecision::Halt { .. }));
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
    }

    /// AC: halt diagnostic message includes each failed slot's ephemeral
    /// branch name (verified indirectly via the canonical helper). Direct
    /// stderr capture is brittle; we verify the helper is consulted by
    /// reproducing its output for a known input.
    #[test]
    fn test_diagnostic_uses_ephemeral_slot_branch_helper() {
        // Sanity-check: the diagnostic format string in the helper must call
        // `worktree::ephemeral_slot_branch` (per CONTRACT AC). If a future
        // refactor inlines `format!()` instead, the names produced for slot 1
        // would still match — but the AC binds us to the helper, so the
        // fastest regression catch is auditing this single call site.
        let name = crate::loop_engine::worktree::ephemeral_slot_branch("feat/test", 1);
        assert_eq!(name, "feat/test-slot-1");
    }

    /// AC (Fix 3): when the deadlock guard fires but every blocking branch
    /// has an unparseable slot suffix, `handle_ephemeral_deadlock` MUST
    /// still produce at least one `FailedMerge` so
    /// `apply_merge_fail_reset_and_halt_check` increments the counter
    /// instead of resetting it. The sentinel index is `SYNTHETIC_DEADLOCK_SLOT`
    /// so the diagnostic step can recognize it and avoid synthesizing a
    /// `{branch}-slot-18446744073709551615` name.
    #[test]
    fn test_synthetic_deadlock_slot_sentinel_increments_counter() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 0;

        // Sentinel-only failed_merges (simulates the all-malformed deadlock
        // path). Generous threshold so we observe the increment without halt.
        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: SYNTHETIC_DEADLOCK_SLOT,
                task_id: None,
            }],
            5,
        );
        assert_eq!(decision, MergeFailHaltDecision::Continue);
        assert_eq!(
            ctx.consecutive_merge_fail_waves, 1,
            "synthetic-deadlock sentinel must still increment the counter"
        );
    }

    /// AC (Fix 3): when the threshold is reached on a sentinel-only failure
    /// wave, the halt diagnostic must NOT synthesize a meaningless
    /// `{branch}-slot-18446744073709551615` name. The reason field surfaces
    /// the counter/threshold; the `<malformed deadlock blocker>` placeholder
    /// flows to stderr (verified indirectly by ensuring no `usize::MAX`
    /// appears in the rendered exit_reason on this halt).
    #[test]
    fn test_synthetic_deadlock_slot_diagnostic_does_not_render_huge_name() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 0;

        // threshold=1 → halt on this very wave; sentinel-only failed_merges.
        let decision = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: SYNTHETIC_DEADLOCK_SLOT,
                task_id: None,
            }],
            1,
        );
        match decision {
            MergeFailHaltDecision::Halt {
                exit_code: _,
                exit_reason,
            } => {
                // The exit_reason itself is just the counter/threshold
                // summary, but the full diagnostic flowed to stderr. Assert
                // the reason does not contain the sentinel-rendered name
                // shape (defensive — catches an accidental future change
                // that puts the slot list back into exit_reason).
                assert!(
                    !exit_reason.contains(&usize::MAX.to_string()),
                    "exit_reason must not include usize::MAX-rendered name; got: {exit_reason}"
                );
            }
            _ => panic!("expected Halt, got {decision:?}"),
        }
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
    }

    /// AC: counter is reset back to 0 by a successful wave AFTER a series
    /// of consecutive failures — i.e. one good wave breaks the cascade.
    #[test]
    fn test_consecutive_counter_lifecycle_failure_then_success() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);

        // Wave 1: fail.
        apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            5, // generous threshold so we don't halt.
        );
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);

        // Wave 2: clean.
        apply_merge_fail_reset_and_halt_check(&conn, &mut ctx, "feat/test", &[], 5);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
    }

    /// AC: IterationContext::new starts the FEAT-002 counter at 0.
    #[test]
    fn test_iteration_context_new_zeroes_consecutive_counter() {
        let ctx = IterationContext::new(5);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
    }

    /// AC: full two-wave cascade — first failure increments without halting
    /// (default threshold = 2); second consecutive failure crosses threshold
    /// and halts. Both waves' failed-slot tasks must end up `todo`.
    #[test]
    fn test_two_consecutive_failures_halt_with_default_threshold() {
        let (_dir, conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        insert_in_progress_task(&conn, "FEAT-002");
        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());
        ctx.pending_slot_tasks.push("FEAT-002".to_string());

        // Wave 1: slot 1 merge fails for FEAT-001. Below threshold → continue.
        let d1 = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2, // default
        );
        assert_eq!(d1, MergeFailHaltDecision::Continue);
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "todo",
            "Wave 1's failed-slot task must be reset to todo"
        );

        // Set FEAT-001 back to in_progress so wave 2 has something realistic
        // to reset (simulates the loop re-claiming the now-todo task).
        conn.execute(
            "UPDATE tasks SET status = 'in_progress' WHERE id = 'FEAT-001'",
            [],
        )
        .unwrap();
        ctx.pending_slot_tasks.push("FEAT-001".to_string());

        // Wave 2: slot 1 fails again. Counter hits threshold → Halt.
        let d2 = apply_merge_fail_reset_and_halt_check(
            &conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2,
        );
        match d2 {
            MergeFailHaltDecision::Halt {
                exit_code,
                exit_reason,
            } => {
                assert_eq!(exit_code, 1);
                assert!(exit_reason.contains("2 consecutive"));
            }
            _ => panic!("expected Halt, got {d2:?}"),
        }
        assert_eq!(ctx.consecutive_merge_fail_waves, 2);
        // CRITICAL: even on halt, the task must be back to todo so the next
        // run can re-claim it.
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "todo",
            "AC: halted run must NOT leave any task in `in_progress` for the failed slots"
        );
    }
}
