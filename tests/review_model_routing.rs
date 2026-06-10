//! Review-class model routing — wiring guards.
//!
//! REFACTOR-005 deleted the legacy post-hoc `reviewModel` override path (the
//! pure-function + bundle-mutation tests that exercised it are removed; their
//! semantic re-coverage under the provider-first `models` + `routing` config is
//! TEST-010's job). What remains here are the source-grep wiring guards that
//! pin the CURRENT contract (FEAT-004): review-class routing flows from rung 3
//! of `resolve_execution_plan` (review → frontier tier), baked into the prompt
//! builder's `resolved_model` before the iteration starts — there is NO
//! post-hoc rewrite at the dispatch sites.

/// The sequential prompt builder resolves the model through the single FR-003
/// path (`resolve_execution_plan`), which carries the review→frontier routing
/// into `resolved_model`; `run_iteration` itself still folds crash escalation
/// and resolves the effective runner.
#[test]
fn run_iteration_routes_review_via_plan() {
    // `run_iteration` was carved into `iteration.rs` (PRD 02, FEAT-004); the
    // sequential body lives there now, not in `engine.rs`.
    let source = std::fs::read_to_string("src/loop_engine/iteration.rs")
        .expect("could not read src/loop_engine/iteration.rs from tests/ cwd");

    let start = source
        .find("pub fn run_iteration(")
        .expect("expected `pub fn run_iteration(` to be defined in iteration.rs");
    let after_open = &source[start..];
    // Find the next top-level fn declaration to mark the body end, falling back
    // to end-of-file: post-carve `run_iteration` is the last (and only) fn in
    // `iteration.rs`, so there is no trailing top-level fn marker.
    let body_end_rel = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
        .iter()
        .filter_map(|marker| {
            after_open[marker.len()..]
                .find(marker)
                .map(|p| p + marker.len())
        })
        .min()
        .unwrap_or(after_open.len());
    let body = &after_open[..body_end_rel];

    // The pre-spawn coordinator and runner resolution remain wired.
    assert!(
        body.contains("resolve_task_execution("),
        "run_iteration MUST still fold crash escalation via resolve_task_execution",
    );
    assert!(
        body.contains("resolve_effective_runner("),
        "run_iteration MUST still resolve the effective runner",
    );

    // The sequential prompt builder resolves through the single FR-003 path,
    // which carries the review→frontier routing into resolved_model.
    let builder = std::fs::read_to_string("src/loop_engine/prompt/sequential.rs")
        .expect("could not read src/loop_engine/prompt/sequential.rs from tests/ cwd");
    assert!(
        builder.contains("resolve_execution_plan("),
        "the sequential prompt builder MUST resolve via resolve_execution_plan — \
         review-class routing flows from its rung 3 (review → frontier tier)",
    );
}

/// The slot prompt builder bakes the review→frontier routing into
/// `SlotPromptBundle::resolved_model` via `resolve_execution_plan` (rung 3)
/// before the bundle crosses the worker boundary — no post-hoc rewrite in
/// `run_wave_iteration`.
#[test]
fn run_wave_iteration_routes_review_via_plan() {
    let builder = std::fs::read_to_string("src/loop_engine/prompt/slot.rs")
        .expect("could not read src/loop_engine/prompt/slot.rs from tests/ cwd");
    assert!(
        builder.contains("resolve_execution_plan("),
        "the slot prompt builder MUST resolve via resolve_execution_plan — \
         review-class routing flows from its rung 3 into SlotPromptBundle::resolved_model",
    );
}
