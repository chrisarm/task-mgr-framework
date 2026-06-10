//! Slot-worker thread-safety source-grep contracts.
//!
//! `IterationContext` is main-thread-only (no interior mutability; not Send-safe
//! by design — Learning #1810). The model-escalation / promotion hooks fire from
//! the post-wave aggregation step on the main thread, NEVER from a parallel slot
//! worker. These source-grep tests pin that wiring so a future refactor cannot
//! quietly move escalation or `IterationContext` mutation into a slot worker.
//!
//! Preserved from the removed `runtime_error_fallback.rs` suite (REFACTOR-006
//! deleted the legacy cross-provider RuntimeError promotion machinery, but these
//! two thread-safety contracts are orthogonal to it and remain load-bearing).

/// `run_slot_iteration` (spawned onto each parallel slot worker thread) must
/// NEVER call `escalate_task_model_if_needed`. The escalation hook fires from
/// the post-wave aggregation step on the main thread (where `IterationContext`
/// lives); calling it from a slot worker would either deadlock on the
/// main-thread `&mut ctx` or silently bypass override insertion.
///
/// We grep the `run_slot_iteration` body span (between its signature and the
/// next top-level helper, `claim_slot_task`) for the call.
#[test]
fn run_slot_iteration_does_not_call_escalate_task_model_if_needed() {
    let source = std::fs::read_to_string("src/loop_engine/slot.rs")
        .expect("could not read src/loop_engine/slot.rs from tests/ cwd");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("expected `pub fn run_slot_iteration(` to be defined in slot.rs");

    let after_open = &source[start..];
    let body_close = after_open
        .find("\npub(super) fn claim_slot_task(")
        .expect("expected `fn claim_slot_task(` after `run_slot_iteration` body");
    let body = &after_open[..body_close];

    assert!(
        !body.contains("escalate_task_model_if_needed"),
        "run_slot_iteration MUST NOT call escalate_task_model_if_needed — \
         the hook is wired on the main thread in the post-wave aggregation step \
         (Learning #1810: IterationContext is not thread-safe). \
         Found call inside run_slot_iteration body. \
         Body span (first 400 chars for diagnosis):\n{}",
        &body[..body.len().min(400)],
    );
}

/// Companion: `run_slot_iteration` must NOT construct or mutate an
/// `IterationContext` (its override maps). That struct is main-thread-only;
/// slot workers receive Send-safe state via `SlotIterationParams` / `SlotContext`.
#[test]
fn run_slot_iteration_does_not_construct_iteration_context() {
    let source = std::fs::read_to_string("src/loop_engine/slot.rs")
        .expect("could not read src/loop_engine/slot.rs from tests/ cwd");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("expected `pub fn run_slot_iteration(` to be defined in slot.rs");
    let after_open = &source[start..];
    let body_close = after_open
        .find("\npub(super) fn claim_slot_task(")
        .expect("expected `fn claim_slot_task(` after `run_slot_iteration`");
    let body = &after_open[..body_close];

    assert!(
        !body.contains("IterationContext::new"),
        "run_slot_iteration MUST NOT construct an IterationContext — that struct \
         is main-thread-only (no Mutex; not Send-safe per design). Slot workers \
         receive Send-safe state via SlotIterationParams / SlotContext only.",
    );
    assert!(
        !body.contains(".runner_overrides")
            && !body.contains(".model_overrides")
            && !body.contains(".effort_overrides"),
        "run_slot_iteration MUST NOT read/write IterationContext override maps — \
         override insertion happens on the main thread in the post-wave aggregation step.",
    );
}
