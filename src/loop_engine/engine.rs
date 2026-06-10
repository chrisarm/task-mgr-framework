use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::commands::init::PrefixMode;
use crate::loop_engine::config::{IterationOutcome, LoopConfig, PermissionMode};
use crate::loop_engine::crash::CrashTracker;
use crate::loop_engine::detection;
use crate::loop_engine::guidance::SessionGuidance;
use crate::loop_engine::model;
use crate::loop_engine::progress;
use crate::loop_engine::project_config;
use crate::loop_engine::runner::RunnerKind;
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::stale::StaleTracker;
use crate::models::RunStatus;

// The slot lifecycle + result-processing functions were carved into `slot.rs`
// (PRD 02, FEAT-001). `run_slot_iteration` is re-exported `pub` so the external
// `task_mgr::loop_engine::engine::run_slot_iteration` import path callers and
// integration tests rely on stays valid (FR-008). Since `run_parallel_wave` /
// `run_wave_iteration` moved to `wave_scheduler.rs` (FEAT-003), the wave call
// sites now import `claim_slot_task` / `process_slot_result` /
// `slot_failure_result` directly from `slot`; the only remaining engine
// consumer of `claim_slot_task` is the inline wave/recovery test modules, so
// its re-export was gated `#[cfg(test)]` to avoid an unused alias in the
// non-test build. After the test-relocation refactor (PRD 02, FEAT-006),
// the only remaining consumer of `claim_slot_task` in tests is `slot.rs`
// itself (via `use super::*`), so the engine re-export is no longer needed.
// `process_slot_result` / `slot_failure_result` are not re-exported here.
pub use crate::loop_engine::slot::run_slot_iteration;

// The sequential per-task iteration body was carved into `iteration.rs` (PRD
// 02, FEAT-004). `run_iteration` is re-exported `pub` so the external import
// path `task_mgr::loop_engine::engine::run_iteration` stays valid (FR-008) and
// `run_loop` (still in this file) keeps calling it by bare name.
pub use crate::loop_engine::iteration::run_iteration;

// The per-task recovery cluster was carved into `recovery.rs` (PRD 02,
// FEAT-002). The public functions are re-exported `pub` so the external import
// paths integration tests and callers rely on
// (`task_mgr::loop_engine::engine::handle_task_failure`, etc.) stay valid
// (FR-008). The three engine-internal helpers (`prompt_overflow_result`,
// `probe_rate_limit_lifted`, `update_trackers`) are consumed by the sequential
// (`iteration.rs`, FEAT-004) and wave (`wave_scheduler.rs`) paths, which import
// them directly from `recovery`, so no `pub(super)` re-export is needed here.
// CLEANUP-001: `auto_block_task` is no longer #[deprecated]; direct export.
pub use crate::loop_engine::recovery::auto_block_task;
// CLEANUP-001: shims removed from recovery.rs; re-export the real relocated
// functions under the old FR-008 external import paths so external tests keep
// compiling. `check_override_invalidation` had no external test caller so its
// re-export is dropped entirely.
pub use crate::loop_engine::reactions::pre_spawn::crash_escalated_model as check_crash_escalation;
pub use crate::loop_engine::recovery::{
    escalate_task_model_if_needed, escalate_task_model_if_needed_for_runner, handle_task_failure,
    handle_task_failure_with_runner, increment_consecutive_failures, reset_consecutive_failures,
    should_auto_block, should_escalate_for_consecutive_failures,
};

// Parallel-wave scheduling + merge-back orchestration was carved into
// `wave_scheduler.rs` (PRD 02, FEAT-003). `run_wave_iteration` /
// `run_parallel_wave` are re-exported `pub` so the external import paths
// integration tests rely on stay valid (FR-008). `run_loop` calls
// `apply_merge_fail_reset_and_halt_check`, `read_prd_implicit_overlap_files`,
// and `reset_task_to_todo` by bare name, so those are re-exported `pub(super)`
// unconditionally. After the test-relocation refactor (PRD 02, FEAT-006),
// `build_slot_contexts`, `apply_post_merge_reconcile`, and `SYNTHETIC_DEADLOCK_SLOT`
// are only referenced by wave_scheduler.rs's own test module (via `use super::*`),
// so no engine.rs re-export is needed for them.
pub(super) use crate::loop_engine::wave_scheduler::{
    apply_merge_fail_reset_and_halt_check, read_prd_implicit_overlap_files, reset_task_to_todo,
};
pub use crate::loop_engine::wave_scheduler::{run_parallel_wave, run_wave_iteration};

// The outer loop orchestration (`run_loop` + run-lifecycle helpers) was carved
// into `orchestrator.rs` (PRD 02, FEAT-005). `run_loop` and `on_run_completed`
// are re-exported `pub` so the external import paths callers
// (`task_mgr::loop_engine::engine::run_loop`, used by `main.rs` / `batch.rs` /
// integration tests) and `on_run_completed` rely on stay valid (FR-008).
pub use crate::loop_engine::orchestrator::{on_run_completed, run_loop};

/// Maximum consecutive reorder attempts before forcing algorithmic pick.
pub(super) const MAX_CONSECUTIVE_REORDERS: u32 = 2;

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
    /// Database connection. `&mut` because the mid-iteration auto-recovery
    /// sweep routes through `TaskLifecycle::recover_in_progress_for_prefix`,
    /// which requires `&mut Connection` to honour the lifecycle SSoT.
    pub conn: &'a mut Connection,
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
    /// Full per-project loop configuration (`.task-mgr/config.json`). Read
    /// once at the start of `run_loop` and threaded through here so that
    /// `overflow::handle_prompt_too_long` (FEAT-006 rung 4) can consult
    /// `fallback_runner` without re-reading the file from every iteration.
    /// Matches the wave-mode plumbing on `WaveIterationParams::project_config`.
    pub project_config: &'a project_config::ProjectConfig,
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
    /// Effective `--effort` level used for this iteration. Carries the plan's
    /// per-provider effort (or the prior-overflow override) — owned `String`
    /// because per-provider effort levels come from runtime config (WIRE-FIX-001).
    /// None when difficulty is unset/unknown or for early exits.
    pub effective_effort: Option<String>,
    /// Effective runner that executed this iteration.
    /// None for pre-dispatch early exits.
    pub effective_runner: Option<RunnerKind>,
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

/// Current wall-clock time as whole seconds since the Unix epoch.
///
/// Single home for the "now" the in-memory quota-blackout channel
/// ([`BlackoutState`]) and its deferral wait are keyed on. A pre-epoch clock
/// (only possible on a badly-misconfigured host) saturates to `0` rather than
/// panicking, so a blackout simply reads as already-expired.
pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// In-memory, self-expiring, provider-keyed quota-blackout channel that lives
/// ONLY on [`IterationContext`] (FR-008). Three load-bearing rules — encoded
/// here so the next reader does not have to reconstruct them:
///
/// 1. **Never persisted.** No DB column, no serde, no disk. A blackout reflects
///    transient account-level quota state, not durable task state. The channel
///    is empty at process start and is **cleared on restart by design** — a
///    fresh loop never inherits a stale blackout.
/// 2. **Never read or written by `promote_once` / `runner_overrides`.** This is
///    the EPHEMERAL per-pass reroute channel; `runner_overrides` is the
///    PERMANENT cross-provider promotion channel owned by
///    `recovery::promote_once`. Mixing them would pin a task to the spillover
///    provider for the rest of the run (the `blackout_reroute_*` known-bad in
///    `tests/model_selection_engine_edges.rs`). The two channels never touch.
/// 3. **Set only by account-level rate-limit signals.** `record` is called
///    exclusively from the post-output rate-limit reaction
///    (`reactions::account::react_to_outputs`); a per-task crash, overflow, or
///    RuntimeError never writes here.
///
/// Each entry maps a [`model::Provider`] to the Unix-epoch second at which its
/// blackout expires. Queries take an explicit `now_secs` (so callers/tests
/// control the clock); expired entries are filtered lazily, so the channel
/// self-clears on expiry without a sweep.
#[derive(Debug, Default)]
pub struct BlackoutState {
    /// provider → Unix-epoch-seconds expiry.
    until: std::collections::HashMap<model::Provider, u64>,
}

impl BlackoutState {
    /// Record `provider` blacked out until `now_secs + reset_secs`. A later
    /// signal for the same provider extends (never shortens) the window.
    pub fn record(&mut self, provider: model::Provider, now_secs: u64, reset_secs: u64) {
        let expiry = now_secs.saturating_add(reset_secs);
        self.until
            .entry(provider)
            .and_modify(|e| *e = (*e).max(expiry))
            .or_insert(expiry);
    }

    /// The set of providers still under blackout at `now_secs` (expired entries
    /// excluded — the self-clearing read). This is the `HashSet<Provider>`
    /// threaded into `model::PlanContext::provider_blackouts`.
    pub fn active(&self, now_secs: u64) -> std::collections::HashSet<model::Provider> {
        self.until
            .iter()
            .filter(|(_, expiry)| **expiry > now_secs)
            .map(|(p, _)| *p)
            .collect()
    }

    /// Whether any provider is still blacked out at `now_secs`.
    pub fn any_active(&self, now_secs: u64) -> bool {
        self.until.values().any(|&expiry| expiry > now_secs)
    }

    /// Longest remaining blackout window in seconds at `now_secs` (`0` when no
    /// entry is still active). Drives the deferral-first wait so it sleeps until
    /// the LAST blacked-out provider reopens.
    pub fn max_remaining_secs(&self, now_secs: u64) -> u64 {
        self.until
            .values()
            .filter(|&&expiry| expiry > now_secs)
            .map(|&expiry| expiry - now_secs)
            .max()
            .unwrap_or(0)
    }

    /// Drop every blackout entry — called after a deferral wait completes so the
    /// next selection pass re-evaluates eligibility against a clean channel.
    pub fn clear(&mut self) {
        self.until.clear();
    }

    /// Whether the channel currently holds no entries at all (used by the
    /// runner-overrides-untouched discriminator and restart semantics).
    pub fn is_empty(&self) -> bool {
        self.until.is_empty()
    }
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
    /// `check_crash_escalation` (updated in the grok-fallback-runner PRD, task
    /// FEAT-007 of that PRD) consults this map directly,
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
    /// Per-task runner overrides. Pinned to the single computed
    /// `effective_runner` at the spawn site (FEAT-005, PRD §2.5). Written by
    /// the overflow rung-4 `FallbackToProvider` (FEAT-008) and the
    /// RuntimeError fallback hook (FEAT-007); read at every spawn site via
    /// [`resolve_effective_runner`]. Empty by default — preserves today's
    /// pure-Claude behavior byte-for-byte (regression guard).
    ///
    /// Loop-thread-local: writes happen on the main thread only. Slot
    /// workers receive a precomputed `RunnerKind` via `SlotContext` and
    /// never touch this map (Learning #1810 thread-safety).
    pub runner_overrides: std::collections::HashMap<String, RunnerKind>,
    /// Per-task snapshot of the `tasks.model` DB column captured at the
    /// moment a runner override was first inserted. Used by
    /// `check_override_invalidation` (FEAT-008) to detect operator edits to
    /// the model column and silently clear stale overrides. `None` value
    /// records that the DB column was NULL at snapshot time, distinct from
    /// the key being absent.
    pub overflow_original_task_model: std::collections::HashMap<String, Option<String>>,
    /// Count of consecutive transient-backend backoff waits without progress
    /// (FEAT-014). Account-global (a backend 5xx / overloaded response is a
    /// shared-account condition, not per-task), so a single scalar — not a
    /// per-task map. Read+written ONLY by the converged
    /// `reactions::account::react_to_transient` (passed as `&mut u32`): reset
    /// to 0 when a wave/iteration carries no `TransientBackend` outcome,
    /// incremented on each `WaitedAndRetry`, and compared against
    /// `TRANSIENT_MAX_ATTEMPTS` to decide when to escalate to the crash/abort
    /// path. Loop-thread-local (the reaction runs on the main thread in both
    /// paths), preserving the no-Mutex contract.
    pub transient_backend_attempts: u32,
    /// In-memory quota-blackout channel (FR-008, FEAT-008). Provider-keyed,
    /// self-expiring, NEVER persisted, and NEVER touched by
    /// `promote_once` / `runner_overrides`. Written only by the account-level
    /// rate-limit reaction (`reactions::account::react_to_outputs`) and read at
    /// the spawn-side resolver (`model::resolve_execution_plan` via
    /// [`BlackoutState::active`]), the quota-deferral wait, and the
    /// excluded-id computation. See [`BlackoutState`] for the three rules.
    pub provider_blackouts: BlackoutState,
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
            runner_overrides: std::collections::HashMap::new(),
            overflow_original_task_model: std::collections::HashMap::new(),
            transient_backend_attempts: 0,
            provider_blackouts: BlackoutState::default(),
        }
    }
}

/// Explicit input to [`resolve_effective_runner`].
///
/// Carries both the effective model string AND the optional `provider_hint`
/// that flowed from the resolved execution plan's provider intent. Production
/// callers MUST construct this struct explicitly so a missed-thread (model-only)
/// call site is a compile-time error rather than a silent Codex→Claude misroute.
///
/// The `From<Option<&str>>` ergonomic conversion (`provider_hint = None`) is
/// `#[cfg(test)]`-gated so tests can stay terse without weakening the
/// production drift guard. Integration tests in the `tests/` crate cannot
/// see `#[cfg(test)]` items in the library and must construct the struct
/// explicitly.
#[derive(Debug, Clone, Copy, Default)]
pub struct EffectiveRunnerInput<'a> {
    /// The final model string after escalation / overrides / review-class
    /// routing — the same string that flows into `--model` at the spawn site.
    pub model: Option<&'a str>,
    /// Explicit provider intent carried from a `primaryRunner` spec match.
    /// `Some(Provider::Codex)` is today the ONLY way Codex is reached.
    /// `None` means "let `provider_for_model(model)` decide".
    pub provider_hint: Option<model::Provider>,
}

#[cfg(test)]
impl<'a> From<Option<&'a str>> for EffectiveRunnerInput<'a> {
    fn from(model: Option<&'a str>) -> Self {
        Self {
            model,
            provider_hint: None,
        }
    }
}

/// Compute the effective runner for a task: per-task override →
/// `provider_hint` → provider of the effective model → default Claude.
///
/// Single source of truth (PRD §2.5): every spawn site MUST resolve runner
/// kind through this helper, never via an OR-style fallback. Re-deriving
/// the formula independently in two places risks silent drift if either
/// branch updates without the other (the prohibition is explicit in the
/// PRD "Prohibited outcomes" list).
///
/// Resolution order (highest → lowest precedence):
/// 1. `ctx.runner_overrides[task_id]` — auto-recovery promotion.
/// 2. `input.provider_hint` — explicit `primaryRunner` intent. **This is the
///    only path that selects [`RunnerKind::Codex`].**
/// 3. `provider_for_model(input.model)` — token-equality classification on the
///    lowercased, hyphen-split model id. Returns only Claude or Grok by
///    design — a `gpt-*`/`o*`/`codex` model id falls through to Claude
///    unless rung 2 already carried a hint.
/// 4. `RunnerKind::Claude` — default for the empty case.
///
/// The helper lives in `engine.rs` (not `runner.rs`) so `runner.rs` stays
/// free of `IterationContext` coupling — the runner module remains
/// provider-neutral.
///
/// The third parameter is `EffectiveRunnerInput<'_>` rather than
/// `impl Into<...>` to make a bare-`Option<&str>` call site a compile error
/// in production — that gap is the silent Codex→Claude misroute vector we
/// defend against (a caller that forgot to thread `provider_hint` would
/// otherwise compile and silently degrade to Claude for a Codex-routed task).
pub fn resolve_effective_runner(
    ctx: &IterationContext,
    task_id: &str,
    input: EffectiveRunnerInput<'_>,
) -> RunnerKind {
    if let Some(kind) = ctx.runner_overrides.get(task_id).copied() {
        return kind;
    }
    let provider = input
        .provider_hint
        .unwrap_or_else(|| model::provider_for_model(input.model));
    // kind-correct: identity translation — maps Provider enum to RunnerKind,
    // two representations of the same provider concept.
    match provider {
        model::Provider::Grok => RunnerKind::Grok,
        model::Provider::Codex => RunnerKind::Codex,
        model::Provider::Claude => RunnerKind::Claude,
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
    /// Pre-resolved runner kind for this slot's task (FEAT-005). Computed on
    /// the main thread via [`resolve_effective_runner`] BEFORE the worker
    /// spawns, so the slot can dispatch without touching
    /// `IterationContext.runner_overrides` (Learning #1810 thread-safety;
    /// the no-override-maps-in-slot-body test enforces this). Default is
    /// `RunnerKind::Claude` for slots constructed without main-thread
    /// enrichment (e.g. test fixtures) — matches today's pure-Claude
    /// behavior byte-for-byte.
    pub effective_runner: RunnerKind,
    /// Prior-overflow effort override for this slot's task (FEAT-002),
    /// resolved on the main thread by `reactions::pre_spawn::resolve_task_execution`
    /// from `IterationContext.effort_overrides` — slot threads must not touch
    /// the override maps (Learning #1810). `run_slot_iteration` prefers this
    /// over `model::effort_for_difficulty(difficulty)`. `None` (the sentinel
    /// default and the no-override case) falls back to the difficulty-derived
    /// effort, so a wave with no overflow history is byte-identical to before.
    /// Closes the audit-#6-effort gap: wave previously dropped this channel.
    pub effective_effort: Option<&'static str>,
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
    /// Pre-resolved runner kind threaded from `SlotContext::effective_runner`.
    /// Used by `process_slot_result` so the overflow rung-4 idempotency guard
    /// pins on the same value the slot used to dispatch (PRD §2.5 single-source
    /// rule). Defaults to `RunnerKind::Claude` for failure entries where no
    /// slot was actually dispatched.
    pub effective_runner: RunnerKind,
    /// Provider hint carried from `SlotPromptBundle::provider_hint` at the
    /// moment `SlotContext::effective_runner` was resolved on the main thread.
    /// Used by the drift-sentinel re-derivation in `process_slot_result` so
    /// the sentinel re-derives via the SAME formula the wave used — not the
    /// bugged formula that dropped the hint when `defaultModel` widened
    /// `resolved_model`. `None` for `slot_failure_result` entries (no bundle).
    pub pre_dispatch_provider_hint: Option<model::Provider>,
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
    /// Tasks directory (where PRD JSON / prompt files live). Threaded to
    /// the protected-state guard so it can confine restore writes to this
    /// root — `db_dir.join("tasks")` is only correct in the common case.
    pub tasks_dir: PathBuf,
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
    /// Loop run ID forwarded to the per-slot grok stderr capture file name.
    /// `None` for non-loop callers; the sniffer uses a placeholder fallback.
    pub run_id: Option<String>,
    /// True when the wave scheduler owns a shared Codex protected-state
    /// snapshot/verification barrier for this wave.
    pub protected_snapshot_active: bool,
}

/// Fields that vary between early-exit paths in `run_slot_iteration`.
///
/// `task_id` is intentionally NOT a field here — it is always the slot's
/// bundle id; threading it through the struct would just invite
/// inconsistencies. `slot_early_exit` reads it from the passed `SlotContext`.
pub(super) struct SlotEarlyExit {
    pub(super) outcome: IterationOutcome,
    pub(super) files_modified: Vec<String>,
    pub(super) should_stop: bool,
    pub(super) output: String,
    pub(super) effective_model: Option<String>,
    pub(super) effective_effort: Option<String>,
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
    /// Default model from the per-project config (`.task-mgr/config.json`),
    /// cached once at `run_loop` startup. Threaded into the post-wave
    /// `handle_task_failure_with_runner` call so the recovery baseline-tier
    /// derivation uses the SAME defaults the primary spawn-site does (FIX-001).
    pub project_default_model: Option<&'a str>,
    /// Default model from the per-user config
    /// (`$XDG_CONFIG_HOME/task-mgr/config.json`), cached at `run_loop` startup.
    /// See `project_default_model` (FIX-001).
    pub user_default_model: Option<&'a str>,
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
    /// Usage API monitoring parameters. Threaded so the converged post-output
    /// rate-limit reaction (`reactions::account::react_to_outputs`, FEAT-006)
    /// can fire the usage wait once per wave — the wave path previously had no
    /// rate-limit handling at all. Mirrors `IterationParams::usage_params`.
    pub usage_params: &'a UsageParams,
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
    /// True only on the FEAT-006 `WaitedAndRetry` early return: the wave hit a
    /// rate limit, waited once, reset its `in_progress` tasks, and bailed out
    /// BEFORE merge-back. The orchestrator must (B3) skip
    /// `apply_merge_fail_reset_and_halt_check` for this wave — calling it with
    /// the empty `failed_merges` this path carries would zero the cascade-halt
    /// streak. Always `false` for every other exit (sequential runs, normal
    /// waves, terminal stops).
    pub rate_limited_retry: bool,
}

/// Wave-mode aggregator collected during per-slot post-processing.
///
/// `all_crashed` starts true only if the wave produced at least one slot
/// outcome — an empty `outcomes` list cannot be "all crashed" — and gets
/// flipped to false the first time a slot doesn't crash (or its claimed
/// task gets marked done despite a crash outcome flag).
pub(super) struct WaveAggregator {
    pub(super) tasks_completed: u32,
    pub(super) any_completed: bool,
    pub(super) all_crashed: bool,
    pub(super) aggregated_files: Vec<String>,
    pub(super) wave_should_stop: bool,
}

impl WaveAggregator {
    // `pub(super)` so `wave_scheduler::run_wave_iteration` (FEAT-003 carve) can
    // construct the aggregator; the type itself stays in `engine.rs`.
    pub(super) fn new(num_outcomes: usize) -> Self {
        Self {
            tasks_completed: 0,
            any_completed: false,
            all_crashed: num_outcomes > 0,
            aggregated_files: Vec::new(),
            wave_should_stop: false,
        }
    }
}

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
#[deprecated(note = "use TaskLifecycle::apply — this shim will be removed in PRD 2 (engine carve)")]
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
    use crate::lifecycle::matrix::TransitionSource;
    use crate::lifecycle::{
        TaskLifecycle, TransitionChange, TransitionIntent, TransitionRejectReason,
    };
    use detection::TaskStatusChange;

    if updates.is_empty() {
        return Vec::new();
    }

    // Convert loop-engine TaskStatusUpdates to lifecycle TransitionIntents.
    // Source is fixed: this shim is only reached from the iteration pipeline,
    // i.e. side-band `<task-status>` tags.
    let intents: Vec<TransitionIntent> = updates
        .iter()
        .map(|u| TransitionIntent {
            task_id: u.task_id.clone(),
            change: match u.status {
                TaskStatusChange::Done => TransitionChange::Done,
                TaskStatusChange::Failed => TransitionChange::Failed,
                TaskStatusChange::Skipped => TransitionChange::Skipped,
                TaskStatusChange::Irrelevant => TransitionChange::Irrelevant,
                TaskStatusChange::Unblock => TransitionChange::Unblock,
                TaskStatusChange::Reset => TransitionChange::Reset,
            },
            source: TransitionSource::LoopStatusTag,
            reason: None,
            fail_status: None,
            audit_note: None,
        })
        .collect();

    let outcomes = {
        let mut lc = match run_id {
            Some(rid) => TaskLifecycle::with_run(conn, rid),
            None => TaskLifecycle::new(conn),
        };
        lc = match (prd_path, task_prefix) {
            (Some(p), Some(prefix)) => lc.with_prd_sync(p, prefix),
            // Calling apply() without a prefix when prd_path is set would
            // skip PRD sync silently — preserve legacy by using "" as
            // prefix when none is supplied (matches `task_prefix: None`
            // behavior of `update_prd_task_passes` via `strip_task_prefix`).
            (Some(p), None) => lc.with_prd_sync(p, ""),
            _ => lc,
        };
        lc.apply(&intents)
    };

    // Loop-engine-specific post-processing the lifecycle service does NOT
    // own (these touch progress files, the per-task crash map, and the
    // dispatch-failed warning stream — all owned by the iteration pipeline,
    // not the status mutation primitive).
    let mut results: Vec<(String, TaskStatusChange, bool)> = Vec::with_capacity(updates.len());
    for (update, outcome) in updates.iter().zip(outcomes.iter()) {
        if outcome.applied {
            // Milestone summary hook — fires on Done success for MILESTONE-*
            // task IDs. Hyphen-anchored to avoid `PRE-MILESTONE-NOTES`-style
            // false matches.
            if matches!(update.status, TaskStatusChange::Done) {
                let is_milestone = update.task_id.contains("-MILESTONE-")
                    || update.task_id.starts_with("MILESTONE-")
                    || update.task_id == "MILESTONE"
                    || update.task_id.ends_with("-MILESTONE");
                if is_milestone && let Some(pp) = progress_path {
                    progress::summarize_milestone(pp, &update.task_id, db_dir);
                }
            }
            // Prune crashed_last_iteration on terminal transitions only —
            // Reset/Unblock leave the row active so the map entry stays.
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
        } else if let Some(TransitionRejectReason::DispatchFailed(msg)) = &outcome.reason {
            // Legacy dispatch-failed warning. Format matches engine.rs's
            // pre-FEAT-003 emit at the previous line ~4821 byte-for-byte:
            // `Warning: <task-status>{id}:{Debug status}</task-status> dispatch failed: {err}`.
            // Migrated to `ui::emit_err` (channel A2): same wire bytes as the
            // prior `eprintln!` (locked stderr + single writeln) so the
            // `lifecycle_stderr_contract.rs` snapshot still passes unchanged.
            crate::output::ui::emit_err(&format!(
                "Warning: <task-status>{}:{:?}</task-status> dispatch failed: {}",
                update.task_id, update.status, msg,
            ));
        }
        results.push((update.task_id.clone(), update.status, outcome.applied));
    }
    results
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
    use crate::loop_engine::runner::RunnerKind;

    // --- FEAT-005: resolve_effective_runner + IterationContext fields ---
    //
    // Regression guards for the single-source effective_runner formula
    // (PRD §2.5). The default-empty IterationContext + no Grok model case
    // MUST resolve to `RunnerKind::Claude` so today's pure-Claude behavior
    // is preserved byte-for-byte. An explicit `runner_overrides` entry
    // wins over the model-derived provider — that's how FEAT-007 / FEAT-008
    // pin a task to Grok once a fallback fires.

    #[test]
    fn feat_005_default_empty_ctx_with_no_model_resolves_to_claude() {
        let ctx = IterationContext::new(8);
        assert_eq!(
            resolve_effective_runner(&ctx, "ANY-TASK-001", None.into()),
            RunnerKind::Claude,
            "default-empty IterationContext with effective_model=None MUST \
             default to ClaudeRunner — preserves pre-FEAT-005 behavior",
        );
    }

    #[test]
    fn feat_005_default_empty_ctx_with_claude_model_resolves_to_claude() {
        let ctx = IterationContext::new(8);
        for model in &[OPUS_MODEL, SONNET_MODEL, HAIKU_MODEL] {
            assert_eq!(
                resolve_effective_runner(&ctx, "TASK-001", Some(*model).into()),
                RunnerKind::Claude,
                "Claude model {model} with empty runner_overrides MUST resolve to Claude",
            );
        }
    }

    #[test]
    fn feat_005_default_empty_ctx_with_grok_model_resolves_to_grok() {
        let ctx = IterationContext::new(8);
        // Token-equality on `-` splits — `grok-build` has token `grok`.
        assert_eq!(
            resolve_effective_runner(&ctx, "TASK-001", Some("grok-build").into()),
            RunnerKind::Grok,
            "Grok model with empty runner_overrides MUST resolve to Grok via \
             provider_for_model token-equality",
        );
        // Groq Inc. (different vendor) MUST NOT mis-route — substring match
        // would catch `groq-llama-3` because `grok` is a substring of `groq`;
        // token-equality correctly rejects it.
        assert_eq!(
            resolve_effective_runner(&ctx, "TASK-001", Some("groq-llama-3").into()),
            RunnerKind::Claude,
            "Groq Inc. model (different vendor) MUST NOT mis-route to Grok",
        );
    }

    #[test]
    fn codex_provider_hint_resolves_to_codex_without_model_auto_routing() {
        let ctx = IterationContext::new(8);
        assert_eq!(
            resolve_effective_runner(
                &ctx,
                "TASK-CODEX",
                EffectiveRunnerInput {
                    model: None,
                    provider_hint: Some(model::Provider::Codex),
                },
            ),
            RunnerKind::Codex,
            "explicit primaryRunner provider intent must route to Codex even when no model is set",
        );
        assert_eq!(
            resolve_effective_runner(&ctx, "TASK-CODEX", Some("codex-mini-latest").into()),
            RunnerKind::Claude,
            "Codex-looking model strings must not auto-route to Codex without provider intent",
        );
    }

    #[test]
    fn feat_005_runner_override_wins_over_model_derived_provider() {
        let mut ctx = IterationContext::new(8);
        ctx.runner_overrides
            .insert("TASK-PINNED".to_string(), RunnerKind::Grok);
        // Model says Claude (Opus), but the override pins to Grok — override wins.
        assert_eq!(
            resolve_effective_runner(&ctx, "TASK-PINNED", Some(OPUS_MODEL).into()),
            RunnerKind::Grok,
            "explicit runner_overrides entry MUST win over the model-derived \
             provider — that's how FEAT-007/FEAT-008 pin a task post-fallback",
        );
        // A different task with no override falls through to the model's provider.
        assert_eq!(
            resolve_effective_runner(&ctx, "TASK-OTHER", Some(OPUS_MODEL).into()),
            RunnerKind::Claude,
            "other tasks without overrides MUST still resolve via the model",
        );
    }

    #[test]
    fn feat_005_iteration_context_new_initializes_runner_fields_empty() {
        let ctx = IterationContext::new(8);
        assert!(
            ctx.runner_overrides.is_empty(),
            "fresh IterationContext.runner_overrides MUST be empty — regression \
             guard against accidental seeded entries that would silently route \
             tasks through a non-Claude runner",
        );
        assert!(
            ctx.overflow_original_task_model.is_empty(),
            "fresh IterationContext.overflow_original_task_model MUST be empty",
        );
    }

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
            effective_runner: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        };
        assert_eq!(result.task_id, Some("FEAT-001".to_string()));
        assert!(!result.should_stop);
    }

    // --- MAX_CONSECUTIVE_REORDERS constant ---

    #[test]
    fn test_max_consecutive_reorders_is_2() {
        assert_eq!(MAX_CONSECUTIVE_REORDERS, 2);
    }
}
