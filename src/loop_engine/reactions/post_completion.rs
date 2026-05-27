//! Post-completion reactions (CONTRACT-001 scaffold; contract pinned by
//! TEST-INIT-004).
//!
//! After a wave's slot results (or the sequential iteration's single result)
//! are processed, completion-driven reactions fire — chiefly triggering any
//! human-review CLARIFY tasks the just-completed work unblocked (#10), folding
//! the external-git completion shadow (#9) and the optional wrapper-commit
//! (#8). The leaf today is `orchestrator::trigger_human_reviews` (a private fn
//! called from `orchestrator.rs` ~L1256). FEAT-010 relocates that body into
//! this module and wires BOTH paths through this coordinator — ADDING the
//! human-review trigger to the wave path (an intentional behavior addition,
//! documented in `src/loop_engine/CLAUDE.md`).
//!
//! ## Input-driven (no completion rediscovery)
//!
//! `react_to_completions` consumes the **already-computed** completed-id set —
//! the ids the shared pipeline + post-merge slot reconcile flipped to `done`
//! this iteration/wave. It does NOT re-query "everything completed since the
//! epoch". That preserves intra-wave ordering: the post-merge reconcile result
//! (`wave_scheduler.rs:1172`) is folded into the id set BEFORE the external-git
//! shadow (`:1196`). Human-review then fires only for `requires_human` tasks
//! whose id is in that set — never for a `requires_human` task that completed
//! out-of-band and is absent from the set.
//!
//! ## Test seam (inner/outer split)
//!
//! `react_to_completions` (production) builds the real review action —
//! `signals::handle_human_review` over stdin, then
//! `prd_reconcile::mutate_prd_from_feedback` on feedback — and delegates to
//! [`react_to_completions_inner`], which takes the review action as an injected
//! [`ReviewFn`] seam. Tests inject a recording spy so they touch no stdin and
//! spawn no Claude subprocess (mirrors `account::{react_to_outputs,
//! react_to_outputs_inner}` + `WaitFn`). The contract is pinned by the ignored
//! TEST-INIT-004 cases in `tests/reaction_parity.rs`; FEAT-010 fills the bodies
//! and removes the `#[ignore]` attributes.

use std::path::Path;

use rusqlite::Connection;

use crate::loop_engine::config::PermissionMode;

/// A `requires_human` task selected from the completed-id set, handed to the
/// [`ReviewFn`] seam. Mirrors the `(id, title, notes, timeout)` tuple
/// `orchestrator::query_human_review_tasks` returns today.
pub struct HumanReviewTask<'a> {
    pub task_id: &'a str,
    pub title: &'a str,
    pub notes: Option<&'a str>,
    pub timeout_secs: Option<u32>,
}

/// Injected human-review seam (inner/outer split, mirrors
/// `account::{react_to_outputs, react_to_outputs_inner}` + `WaitFn`).
///
/// Called **exactly once per `requires_human` completed task** with the
/// selected [`HumanReviewTask`]; returns `true` when the reviewer supplied
/// feedback (production then calls `mutate_prd_from_feedback`), `false`
/// otherwise. Production wires `signals::handle_human_review`; tests inject a
/// recording spy so they are hermetic (no stdin, no subprocess). A type alias
/// keeps `clippy::type_complexity` quiet.
pub type ReviewFn<'f> = &'f dyn Fn(HumanReviewTask<'_>) -> bool;

/// Inputs to [`react_to_completions`]. Destructured exhaustively (no `..`) by
/// the FEAT-010 body — adding a field is a compile error until the coordinator
/// accounts for it (CONTRACT-001 parity lock).
pub struct PostCompletionParams<'a> {
    /// Active run id (FK for any completion bookkeeping the body adds).
    pub run_id: &'a str,
    /// 1-based loop iteration (passed through to `handle_human_review`).
    pub iteration: u32,
    /// Worktree root — wrapper-commit target + the external-git completion scan.
    pub working_root: &'a Path,
    /// PRD JSON path — `mutate_prd_from_feedback` target on review feedback.
    pub prd_file: &'a Path,
    /// Active task prefix (scopes the external-git reconcile + PRD mutation).
    pub task_prefix: Option<&'a str>,
    /// Default model for the PRD-mutation sub-agent.
    pub default_model: Option<&'a str>,
    /// Permission mode for the PRD-mutation sub-agent.
    pub permission_mode: &'a PermissionMode,
    /// External git repo to scan for the completion shadow (#9), if configured.
    pub external_repo_path: Option<&'a Path>,
    /// Commit-scan depth for the external-git shadow.
    pub external_git_scan_depth: u32,
    /// Wrapper-commit knob (#8): `true` on the sequential path (commit on the
    /// task's behalf when Claude couldn't), `false` on the wave path (slot
    /// merge-back already carries the commit).
    pub wrapper_commit: bool,
}

/// Post-completion coordinator (production entry point). Builds the real review
/// action (stdin `handle_human_review` + `mutate_prd_from_feedback`) and
/// delegates to [`react_to_completions_inner`].
///
/// Folds the completion-driven reactions for the wave's N results or the
/// sequential path's 1, consuming the **provided** `completed_ids` set so the
/// intra-wave ordering (post-merge reconcile before external-git shadow) is
/// preserved.
///
/// **Scaffold under TEST-INIT-004** — body implemented by FEAT-010.
#[allow(dead_code)] // wired into both paths by FEAT-010
pub fn react_to_completions(
    conn: &mut Connection,
    completed_ids: &[String],
    params: &PostCompletionParams<'_>,
) {
    let _ = (conn, completed_ids, params);
    unimplemented!(
        "FEAT-010: build the real review action (handle_human_review + \
         mutate_prd_from_feedback) and delegate to react_to_completions_inner"
    )
}

/// Hermetic core of the post-completion coordinator. Destructures the params
/// exhaustively; for each id in `completed_ids` that is `requires_human = 1`
/// AND `status = 'done'`, fires `review` **exactly once** with its
/// [`HumanReviewTask`]. Does NOT review a `requires_human` task whose id is
/// absent from `completed_ids` (input-driven, no timestamp rediscovery), and
/// folds the external-git completion shadow + the `wrapper_commit` knob.
///
/// **Scaffold under TEST-INIT-004** — body implemented by FEAT-010. The
/// contract is pinned by the ignored tests in `tests/reaction_parity.rs`.
#[allow(dead_code)] // invoked by react_to_completions; pinned by TEST-INIT-004
pub fn react_to_completions_inner(
    conn: &mut Connection,
    completed_ids: &[String],
    params: &PostCompletionParams<'_>,
    review: ReviewFn<'_>,
) {
    let _ = (conn, completed_ids, params, review);
    unimplemented!(
        "FEAT-010: destructure PostCompletionParams exhaustively; for each id in \
         completed_ids that is requires_human=1 AND status='done', fire `review` \
         EXACTLY once with its HumanReviewTask; do NOT review a requires_human \
         task whose id is absent from completed_ids (input-driven, no timestamp \
         rediscovery); fold the external-git completion shadow + wrapper_commit"
    )
}
