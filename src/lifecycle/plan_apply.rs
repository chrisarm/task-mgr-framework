//! Shared `apply_plan_with_source` helper — used by
//! [`TaskLifecycle::reconcile_from_prd`] and [`TaskLifecycle::repair_stale`].
//!
//! Both verbs consume a flat plan (`Vec<ReconcileItem>` / `Vec<RepairItem>`,
//! both `{task_id, target, audit_label}`) and dispatch through the same
//! per-item engine: matrix-derived `WHERE status IN (...)` atomic UPDATE,
//! best-effort PRD JSON sync, and a fallback SELECT that disambiguates
//! `skipped` (from == target idempotent no-op) from `rejected` (matrix-
//! disallowed or missing row) only on the rare `rows_affected == 0` branch.
//!
//! Per FR-007 and the architect-revised PRD §6 doctor sub-decision, the two
//! verbs stay distinct at the API boundary — only the implementation of
//! plan-application is shared here. Plan-building remains in
//! `loop_engine/prd_reconcile.rs` and `commands/doctor/fixes.rs`.

use rusqlite::types::Value;

use crate::TaskMgrError;
use crate::loop_engine::prd_reconcile::update_prd_task_passes;
use crate::models::TaskStatus;

use super::TaskLifecycle;
use super::matrix::{self, TransitionSource};
use super::reconcile::ReconcileItem;
use super::repair::RepairItem;

/// Common read-only view over the per-item fields needed by
/// `apply_plan_with_source`. Both [`ReconcileItem`] and [`RepairItem`] expose
/// the same shape; this trait lets the shared helper iterate either without
/// allocating a re-shaped intermediate vec.
pub(crate) trait PlanItemView {
    fn task_id(&self) -> &str;
    fn target(&self) -> TaskStatus;
    fn audit_label(&self) -> Option<&str>;
}

impl PlanItemView for ReconcileItem {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn target(&self) -> TaskStatus {
        self.target
    }
    fn audit_label(&self) -> Option<&str> {
        self.audit_label.as_deref()
    }
}

impl PlanItemView for RepairItem {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn target(&self) -> TaskStatus {
        self.target
    }
    fn audit_label(&self) -> Option<&str> {
        self.audit_label.as_deref()
    }
}

/// Internal aggregate returned by [`TaskLifecycle::apply_plan_with_source`].
/// Both `ReconcileReport` and `RepairReport` are thin wrappers over this
/// shape.
#[derive(Debug, Default)]
pub(crate) struct PlanReport {
    pub(crate) applied: usize,
    pub(crate) skipped: usize,
    pub(crate) rejected: Vec<String>,
}

impl<'a> TaskLifecycle<'a> {
    /// Execute a flat plan under `source`.
    ///
    /// Per-item flow (race-safe, single round-trip on the happy path):
    /// 1. Look up the matrix-permitted starting states for `(target, source)`
    ///    via [`matrix::allowed_from_for_plan`]. Empty slice → push to
    ///    `rejected`, continue.
    /// 2. Execute one atomic UPDATE with `WHERE id = ? AND status IN (?, ?, …)`
    ///    bound from the matrix slice. The notes column uses a `CASE WHEN`
    ///    expression so the audit-label append is inline — no SELECT-then-
    ///    UPDATE round-trip on either status or notes (preserves the
    ///    PRD-prohibited-outcome contract that bans new round-trips where
    ///    the legacy SQL was a single conditional UPDATE).
    /// 3. `rows_affected == 1` → applied. On `target == Done` AND
    ///    [`TaskLifecycle::with_prd_sync`] configured, fire
    ///    `update_prd_task_passes`. Failures emit the same stderr line shape
    ///    as [`Self::apply`] and DO NOT toggle `applied` to `skipped`/
    ///    `rejected` — the DB write is authoritative, PRD JSON is
    ///    best-effort (the legacy invariant locked by
    ///    `tests/lifecycle_stderr_contract.rs`).
    /// 4. `rows_affected == 0` → fallback ONE SELECT to disambiguate
    ///    idempotent skips from matrix rejections: `from == target` →
    ///    `skipped` (the row was already at the target); otherwise →
    ///    `rejected` (matrix-disallowed status, missing row, or rare race
    ///    where the row advanced to a non-target state between the matrix
    ///    check and the UPDATE).
    ///
    /// Per-item partial-failure tolerance: a single item's failure NEVER
    /// aborts the batch. Failures land in `rejected`; iteration continues.
    pub(crate) fn apply_plan_with_source<T: PlanItemView>(
        &mut self,
        items: &[T],
        source: TransitionSource,
    ) -> Result<PlanReport, TaskMgrError> {
        let mut report = PlanReport::default();

        for item in items {
            let task_id = item.task_id();
            let target = item.target();
            let audit_label = item.audit_label();

            let allowed = matrix::allowed_from_for_plan(target, source);
            if allowed.is_empty() {
                report.rejected.push(task_id.to_string());
                continue;
            }

            let rows = match execute_plan_update(self.conn, task_id, target, allowed, audit_label) {
                Ok(n) => n,
                Err(_) => {
                    report.rejected.push(task_id.to_string());
                    continue;
                }
            };

            if rows > 0 {
                report.applied += 1;
                if target == TaskStatus::Done
                    && let (Some(path), Some(prefix)) = (self.prd_json_path, self.task_prefix)
                    && let Err(e) = update_prd_task_passes(path, task_id, true, Some(prefix))
                {
                    // Stderr shape mirrors `TaskLifecycle::apply` —
                    // operators grep for the "PRD JSON sync failed"
                    // substring (TEST-INIT-003 contract).
                    eprintln!(
                        "Warning: <task-status> dispatched {} to done in DB but PRD JSON sync failed ({}): {}",
                        task_id,
                        path.display(),
                        e,
                    );
                }
            } else {
                // rows_affected == 0 — fallback SELECT decides skipped vs
                // rejected. This is the ONLY SELECT in apply_plan_with_source,
                // and it fires only on the rare unhappy path (idempotent
                // no-op, matrix-disallowed status, missing row, or race).
                match super::read_status(self.conn, task_id) {
                    Some(from) if from == target => report.skipped += 1,
                    _ => report.rejected.push(task_id.to_string()),
                }
            }
        }

        Ok(report)
    }
}

/// Emit one atomic UPDATE for `(task_id, target)` constrained to the
/// matrix-permitted `allowed_from` set via a `WHERE id = ? AND status IN
/// (?, ?, …)` clause. Returns `rows_affected`.
///
/// The notes column is updated via a `CASE WHEN`-based append so the
/// audit-label codepath needs no SELECT notes round-trip (matches the
/// FEAT-001 decay_reset pattern). `audit_label = None` binds SQL NULL and
/// the CASE preserves existing notes byte-identically.
///
/// Targets outside `{Done, Todo, Irrelevant}` return `Ok(0)` defensively —
/// no plan-driven caller (reconcile / repair) produces other targets, and
/// the matrix gate above is the true allowlist.
fn execute_plan_update(
    conn: &rusqlite::Connection,
    task_id: &str,
    target: TaskStatus,
    allowed_from: &[TaskStatus],
    audit_label: Option<&str>,
) -> Result<usize, TaskMgrError> {
    // Target-specific column writes. The matrix gate guarantees these are
    // the only targets reconcile/repair produce; other targets short-circuit
    // to 0 rows rather than panic.
    let extra_set = match target {
        TaskStatus::Done => ", completed_at = datetime('now')",
        TaskStatus::Todo => ", started_at = NULL",
        TaskStatus::Irrelevant => "",
        _ => return Ok(0),
    };
    let target_str = target.as_db_str();

    let in_placeholders = std::iter::repeat_n("?", allowed_from.len())
        .collect::<Vec<_>>()
        .join(", ");

    // Single statement: status flip + target-specific column + inline
    // CASE WHEN notes append (no SELECT notes round-trip). Three binds for
    // the audit-label expression (NULL probe + two value branches).
    let sql = format!(
        "UPDATE tasks SET status = '{target_str}'{extra_set}, \
         notes = CASE \
                 WHEN ? IS NULL THEN notes \
                 WHEN notes IS NULL OR notes = '' THEN ? \
                 ELSE notes || char(10) || char(10) || ? END, \
         updated_at = datetime('now') \
         WHERE id = ? AND status IN ({in_placeholders})"
    );

    // Use rusqlite::types::Value so a heterogeneous param vec (NULL probe +
    // two audit-label binds + task_id + N status strings) can flow through
    // a single params_from_iter call. Matches the variadic-IN pattern
    // already used in `claim.rs::try_claim` (learning #3321).
    let label_value = match audit_label {
        Some(s) => Value::from(s.to_string()),
        None => Value::Null,
    };
    let mut params: Vec<Value> = Vec::with_capacity(4 + allowed_from.len());
    params.push(label_value.clone());
    params.push(label_value.clone());
    params.push(label_value);
    params.push(Value::from(task_id.to_string()));
    for s in allowed_from {
        params.push(Value::from(s.as_db_str().to_string()));
    }

    let rows = conn.execute(&sql, rusqlite::params_from_iter(params))?;
    Ok(rows)
}
