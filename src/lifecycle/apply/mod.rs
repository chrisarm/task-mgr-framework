//! `TaskLifecycle::apply` — Category A user-intent + LoopStatusTag dispatch.
//!
//! Owns the side effects today scattered across `commands/{complete,
//! fail/transition, skip, irrelevant, unblock, reset, review}.rs` plus the
//! `apply_status_updates` dispatcher at `loop_engine/engine.rs:4697`.
//!
//! Per-task partial-failure tolerance is a HARD contract (learning #2284):
//! `apply()` MUST return one `TransitionOutcome` per input intent, in input
//! order. NEVER fold this into a batch-level `Result<(), Err>`.

mod complete;
mod fail;
mod irrelevant;
mod reset;
mod skip;
mod unblock;
mod unskip;

use rusqlite::params;

use crate::TaskMgrResult;
use crate::cli::FailStatus;
use crate::loop_engine::prd_reconcile::update_prd_task_passes;
use crate::models::TaskStatus;

use super::TaskLifecycle;
use super::matrix::TransitionSource;

/// Intent semantics — what action the caller is requesting on a task.
///
/// Mirrors `loop_engine::detection::TaskStatusChange` deliberately. The two
/// types stay structurally distinct (lifecycle is a lower layer than the loop
/// engine and must not depend on it for type modeling); the iteration
/// pipeline converts at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionChange {
    /// Mark task complete (`-> Done`).
    Done,
    /// Fail the task (default `-> Blocked`; `FailStatus` variant controls the
    /// exact terminal). Maps to `commands::fail`.
    Failed,
    /// Skip for later (`InProgress -> Skipped`).
    Skipped,
    /// Mark obsolete (`InProgress -> Irrelevant`).
    Irrelevant,
    /// Return a blocked task to todo (`Blocked -> Todo`).
    Unblock,
    /// Return a skipped task to todo (`Skipped -> Todo`).
    Unskip,
    /// Reset to todo from any non-terminal state (`* -> Todo`).
    Reset,
}

impl TransitionChange {
    /// The terminal status this change requests. Default `FailStatus` for
    /// [`TransitionChange::Failed`] is `Blocked`, matching today's
    /// `apply_status_updates` (`engine.rs:4751`).
    #[must_use]
    pub fn target(self) -> TaskStatus {
        match self {
            Self::Done => TaskStatus::Done,
            Self::Failed => TaskStatus::Blocked,
            Self::Skipped => TaskStatus::Skipped,
            Self::Irrelevant => TaskStatus::Irrelevant,
            Self::Unblock => TaskStatus::Todo,
            Self::Unskip => TaskStatus::Todo,
            Self::Reset => TaskStatus::Todo,
        }
    }
}

/// One transition request handed to `apply()`. Carries the source so the
/// matrix validator can apply source-specific allowances (see PRD §6).
#[derive(Debug, Clone)]
pub struct TransitionIntent {
    pub task_id: String,
    pub change: TransitionChange,
    pub source: TransitionSource,
    /// Optional human-readable reason; threaded into audit notes today via
    /// the build_notes() helpers in the per-verb command modules.
    pub reason: Option<String>,
    /// For [`TransitionChange::Failed`] only: which terminal to land on
    /// (`Blocked` / `Skipped` / `Irrelevant`). `None` defaults to
    /// `FailStatus::Blocked` (the LoopStatusTag side-band shape). For any
    /// other `TransitionChange` this field is ignored.
    pub fail_status: Option<FailStatus>,
    /// Optional audit-note override. When `Some`, used verbatim by the
    /// internal helpers (callers that need custom audit prefixes like
    /// `[AUTO-UNBLOCKED]` / `[RESOLVED]` set this). When `None`, the
    /// internal helper builds the default audit note for that variant.
    pub audit_note: Option<String>,
}

/// Per-intent outcome. Returned in input order from `apply()`, one outcome
/// per input intent regardless of success.
///
/// Fields carry enough information for callers to:
/// - prune terminal-status entries from tracking maps (learnings #2796,
///   #2304): inspect `target.is_terminal() && applied`,
/// - drive the per-task `:done` completion gate at
///   `iteration_pipeline.rs:286` (learning #2238): inspect
///   `applied && target == TaskStatus::Done`,
/// - convert back to the legacy `(task_id, change, applied)` tuple shape
///   that the engine.rs shim must keep returning to existing callers.
#[derive(Debug, Clone)]
pub struct TransitionOutcome {
    pub task_id: String,
    /// The terminal the intent requested (derived from `intent.change`).
    pub target: TaskStatus,
    /// Status read from `tasks` immediately before any side effect ran.
    /// `None` when the row was missing or `status` failed to parse.
    /// For the auto-claim path this remains `Some(Todo)` — `apply()` reports
    /// the *original* previous, not the intermediate `InProgress` it set.
    pub previous: Option<TaskStatus>,
    /// `true` iff the underlying dispatcher returned `Ok(_)`. PRD JSON sync
    /// failures DO NOT flip `applied` to false — DB is authoritative.
    pub applied: bool,
    /// Populated only when `applied = false`.
    pub reason: Option<TransitionRejectReason>,
}

/// Why a transition was rejected. Distinct variants let callers branch on
/// matrix-rejection vs. dispatch-failure vs. lookup-failure without parsing
/// error strings.
#[derive(Debug, Clone)]
pub enum TransitionRejectReason {
    /// The `(from, to, source)` triple is not allowed by the matrix.
    InvalidTransition {
        from: TaskStatus,
        to: TaskStatus,
        source: TransitionSource,
    },
    /// The task ID was not found in the `tasks` table.
    TaskNotFound,
    /// The task is in a terminal state and the requested transition is not
    /// permitted for the source.
    TerminalState { from: TaskStatus },
    /// The matrix accepted the triple but the underlying DB write or side
    /// effect (run_tasks insert, PRD JSON sync, etc.) failed. The string
    /// carries the displayed `TaskMgrError` for stderr / progress logging.
    ///
    /// **Why a `String` and not a typed `TaskMgrError`?** Three constraints
    /// converge: (1) `TaskMgrError` does not implement `Clone` because
    /// `rusqlite::Error` is not `Clone`, so it cannot be stored in the
    /// `Clone`-derived `TransitionRejectReason`; (2) all five call sites that
    /// pattern-match this variant extract the message verbatim via
    /// `lock_error_with_hint()` — none reconstruct typed error variants;
    /// (3) the original design never required typed reconstruction at the
    /// caller boundary. Adding structure here would require `Arc<TaskMgrError>`
    /// (hot-path heap alloc) or a new `DispatchKind` enum — both are
    /// explicitly deferred to a future PRD.
    DispatchFailed(String),
}

impl<'a> TaskLifecycle<'a> {
    /// Apply a batch of transitions.
    ///
    /// Returns one outcome per input intent, in input order. A single
    /// intent's failure NEVER short-circuits the batch — that contract is
    /// what `apply_status_updates` already guarantees today (learning
    /// #2284 / #2238).
    ///
    /// Empty `intents` returns `Vec::new()` with no DB round-trip (the
    /// "no transaction commit" AC).
    ///
    /// Side effects (per-intent, in order):
    /// 1. **Auto-claim**: for [`TransitionSource::LoopStatusTag`] +
    ///    [`TransitionChange::Done`] with previous `Todo`, transition
    ///    `Todo -> InProgress` first; when `run_id` is configured via
    ///    [`Self::with_run`], also `INSERT OR IGNORE` a `run_tasks` row with
    ///    `iteration = MAX(iteration)+1` (mirrors `engine.rs:4730-4742`
    ///    byte-for-byte).
    /// 2. **Dispatch**: route to the matching command handler
    ///    (`complete` / `fail` / `skip` / `irrelevant` / `unblock` / `reset`).
    ///    Each handler manages its own transaction internally; this preserves
    ///    per-task partial-failure isolation (one task's rollback never
    ///    touches another).
    /// 3. **PRD JSON sync**: on `Done` success, when both `prd_json_path`
    ///    and `task_prefix` are configured via [`Self::with_prd_sync`], call
    ///    `update_prd_task_passes`. Failure emits a stderr warning matching
    ///    the legacy bytes locked by `TEST-INIT-003`
    ///    (`tests/lifecycle_stderr_contract.rs`) and DOES NOT abort the DB
    ///    write (`applied` stays `true` — DB-authoritative, PRD best-effort).
    pub fn apply(&mut self, intents: &[TransitionIntent]) -> Vec<TransitionOutcome> {
        if intents.is_empty() {
            return Vec::new();
        }
        let mut results = Vec::with_capacity(intents.len());
        for intent in intents {
            results.push(self.apply_one(intent));
        }
        results
    }

    fn apply_one(&mut self, intent: &TransitionIntent) -> TransitionOutcome {
        let target = intent.change.target();
        let previous = super::read_status(self.conn, &intent.task_id);

        // Matrix gate for Done transitions ONLY. The other internal helpers
        // own their own per-variant validation (skip_one rejects Done origin,
        // unblock_one requires Blocked, reset_one requires non-Todo, etc.).
        // The Done gate catches the Operator/Todo→Done case (legacy
        // can_transition_to in complete.rs:199) without forcing the helper
        // dispatchers to re-check, and skips auto-claimable LoopStatusTag
        // Todo→Done (where the row is about to advance through InProgress).
        if matches!(intent.change, TransitionChange::Done)
            && let Some(prev) = previous
            && !(matches!(intent.source, TransitionSource::LoopStatusTag)
                && prev == TaskStatus::Todo)
            && let Err(reject) = super::matrix::validate(prev, target, intent.source)
        {
            return TransitionOutcome {
                task_id: intent.task_id.clone(),
                target,
                previous,
                applied: false,
                reason: Some(reject),
            };
        }

        // Auto-claim: LoopStatusTag Done from Todo silently advances the row
        // through InProgress before the Done dispatch.
        if matches!(intent.source, TransitionSource::LoopStatusTag)
            && matches!(intent.change, TransitionChange::Done)
            && previous == Some(TaskStatus::Todo)
        {
            // Ignore DB errors — treat auto-claim failure as a no-op and let
            // the downstream dispatch surface any real problem.
            let _ = self.do_auto_claim(&intent.task_id);
        }

        // Matrix is consulted for Done only; other variants own their inline checks — see matrix.rs §"Matrix consultation policy".
        // The `previous` status (read once in apply_one) is passed to verbs that can use it
        // either for validation or to avoid a redundant status SELECT. Verbs that only need
        // it for cosmetic audit labels tolerate `None`. Verbs that perform their own strict
        // validation (e.g. reset) fall back to a real read+parse when `None` to preserve
        // fail-fast behavior on corrupt data.
        let dispatch = match intent.change {
            TransitionChange::Done => self.complete_one(intent, previous),
            TransitionChange::Failed => {
                let fail_status = intent.fail_status.unwrap_or(FailStatus::Blocked);
                self.fail_one(intent, fail_status)
            }
            TransitionChange::Skipped => self.skip_one(intent, previous),
            TransitionChange::Irrelevant => self.irrelevant_one(intent, previous),
            TransitionChange::Unblock => self.unblock_one(intent, previous),
            TransitionChange::Unskip => self.unskip_one(intent),
            TransitionChange::Reset => self.reset_one(intent, previous),
        };

        let (applied, reason) = match dispatch {
            Ok(()) => (true, None),
            Err(ref e) => (
                false,
                Some(TransitionRejectReason::DispatchFailed(e.to_string())),
            ),
        };

        // PRD JSON sync — only on Done success, only when both
        // prd_json_path and task_prefix are configured. Failures emit the
        // legacy stderr line locked by TEST-INIT-003 and DO NOT toggle
        // `applied` (DB-authoritative, PRD best-effort).
        if applied
            && matches!(intent.change, TransitionChange::Done)
            && let (Some(path), Some(prefix)) = (self.prd_json_path, self.task_prefix)
            && let Err(e) = update_prd_task_passes(path, &intent.task_id, true, Some(prefix))
        {
            eprintln!(
                "Warning: <task-status> dispatched {} to done in DB but PRD JSON sync failed ({}): {}",
                intent.task_id,
                path.display(),
                e,
            );
        }

        TransitionOutcome {
            task_id: intent.task_id.clone(),
            target,
            previous,
            applied,
            reason,
        }
    }

    /// Attempt to advance `task_id` from `Todo → InProgress` and, when the
    /// UPDATE matched, insert a `run_tasks` row.
    ///
    /// Returns `true` iff the UPDATE matched (conditional WHERE hit exactly one
    /// row). Propagates DB errors via `?` so the caller can decide how to
    /// handle them.
    fn do_auto_claim(&mut self, task_id: &str) -> TaskMgrResult<bool> {
        // Conditional WHERE matches engine.rs:4723-4729 exactly: the row only
        // advances when it is still 'todo' at UPDATE time.
        // Auto-claim's conditional WHERE may no-op under concurrent status flip;
        // don't record an orphan run_tasks row in that case.
        let claimed = self.conn.execute(
            "UPDATE tasks SET status = 'in_progress', \
             started_at = datetime('now'), \
             updated_at = datetime('now') \
             WHERE id = ? AND status = 'todo'",
            [task_id],
        )? == 1;
        if claimed && let Some(rid) = self.run_id {
            // MAX(iteration)+1 mirrors engine.rs:4732-4737 — compute the
            // index from the DB to keep the run_tasks sequence
            // byte-identical to the legacy behavior.
            let next_iter: i64 = self
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(iteration), 0) + 1 FROM run_tasks WHERE run_id = ?",
                    [rid],
                    |row| row.get(0),
                )
                .unwrap_or(1);
            self.conn.execute(
                "INSERT OR IGNORE INTO run_tasks (run_id, task_id, iteration, status) \
                 VALUES (?, ?, ?, 'started')",
                params![rid, task_id, next_iter],
            )?;
        }
        Ok(claimed)
    }
}
