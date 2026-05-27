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

use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::guidance::SessionGuidance;
use crate::loop_engine::{git_reconcile, prd_reconcile, signals};

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

/// Outcome of [`react_to_completions`] / [`react_to_completions_inner`] that the
/// caller folds into its own iteration/wave accounting.
#[derive(Debug, Default)]
pub struct PostCompletionOutcome {
    /// Task ids newly marked done by the external-git completion shadow (#9)
    /// during this call. Empty when no external repo is configured or nothing
    /// new matched. The caller bumps `tasks_completed`, flips its outcome /
    /// crash-tracker to "completed", and clears any claimed/pending task these
    /// ids resolved — the same accounting the pre-convergence inline external-git
    /// block did at each call site.
    pub external_reconciled: Vec<String>,
    /// The hash of the last wrapper-commit (#8) made on a completed task's
    /// behalf, or `None` when `wrapper_commit` was `false`, nothing was dirty, or
    /// the commit failed. The sequential caller stores it in `ctx.last_commit`;
    /// the wave caller ignores it (`wrapper_commit = false`).
    pub wrapper_commit_hash: Option<String>,
}

/// Post-completion coordinator (production entry point). Builds the real review
/// action (stdin `handle_human_review` + `mutate_prd_from_feedback`) and
/// delegates to [`react_to_completions_inner`].
///
/// Folds the completion-driven reactions for the wave's N results or the
/// sequential path's 1, consuming the **provided** `completed_ids` set so the
/// intra-wave ordering (post-merge reconcile before external-git shadow) is
/// preserved. BOTH execution paths route through this coordinator: the
/// sequential path from `orchestrator::run_loop` (`wrapper_commit = true`), the
/// wave path from `wave_scheduler::run_wave_iteration` (`wrapper_commit = false`
/// — the slot merge-back already carries the commit), which ADDS human review to
/// the wave path: an intentional behavior addition documented in
/// `src/loop_engine/CLAUDE.md`.
///
/// `session_guidance` is threaded separately (not on [`PostCompletionParams`])
/// because it is the mutable conversation state `handle_human_review` appends to
/// — the same `&mut SessionGuidance` the pre-convergence
/// `orchestrator::trigger_human_reviews` took. The production review closure
/// captures it and, on feedback, records the text; `mutate_prd_from_feedback` is
/// applied AFTER [`react_to_completions_inner`] returns so the `&mut Connection`
/// it borrows is free.
pub fn react_to_completions(
    conn: &mut Connection,
    completed_ids: &[String],
    params: &PostCompletionParams<'_>,
    session_guidance: &mut SessionGuidance,
) -> PostCompletionOutcome {
    // Feedback text captured per reviewed task whose reviewer supplied guidance.
    // Deferred (not applied inside the closure) because `mutate_prd_from_feedback`
    // needs `&mut Connection`, which `react_to_completions_inner` borrows.
    let pending_feedback: RefCell<Vec<String>> = RefCell::new(Vec::new());
    // `RefCell` because `ReviewFn` is `Fn` (not `FnMut`) yet `handle_human_review`
    // mutably appends to the guidance — mirrors the `WaitFn` seam in `account`.
    let guidance = RefCell::new(session_guidance);

    let outcome = {
        let review = |task: HumanReviewTask<'_>| -> bool {
            let mut sg = guidance.borrow_mut();
            let had_feedback = signals::handle_human_review(
                io::BufReader::new(io::stdin()),
                task.task_id,
                task.title,
                task.notes,
                params.iteration,
                &mut sg,
                task.timeout_secs,
            );
            if had_feedback {
                let feedback = sg.last_text().unwrap_or("").to_string();
                pending_feedback.borrow_mut().push(feedback);
            }
            had_feedback
        };
        react_to_completions_inner(conn, completed_ids, params, &review)
    };

    // Apply downstream PRD mutation for every review that produced feedback, now
    // that the inner call's `&mut Connection` borrow has been released.
    for feedback in pending_feedback.into_inner() {
        prd_reconcile::mutate_prd_from_feedback(
            params.prd_file,
            &feedback,
            conn,
            params.task_prefix,
            params.default_model,
            params.permission_mode,
        );
    }

    outcome
}

/// Hermetic core of the post-completion coordinator. Destructures the params
/// exhaustively; folds the three completion-driven reactions in order:
///
/// 1. **Wrapper-commit (#8)** — when `wrapper_commit` is set, commit on each
///    completed task's behalf (the slot merge-back covers this on the wave path,
///    so it passes `false`). `wrapper_commit`'s own `git status --porcelain`
///    check makes the call a no-op once the tree is clean, so at most one commit
///    lands per iteration.
/// 2. **External-git completion shadow (#9)** — scan the configured external repo
///    for `<id>-completed` markers and mark matches done. Runs AFTER the caller
///    fed `completed_ids` from the post-merge reconcile (AC5); its reconciled ids
///    extend the human-review set.
/// 3. **Human review (#10)** — for each id in `completed_ids` ∪
///    `external_reconciled` that is `requires_human = 1` AND `status = 'done'`,
///    fire `review` **exactly once** with its [`HumanReviewTask`]. A
///    `requires_human` task whose id is absent from the set is NEVER reviewed
///    (input-driven, no timestamp rediscovery) — this is what preserves the
///    intra-wave ordering.
///
/// The contract is pinned by the parity tests in `tests/reaction_parity.rs`.
pub fn react_to_completions_inner(
    conn: &mut Connection,
    completed_ids: &[String],
    params: &PostCompletionParams<'_>,
    review: ReviewFn<'_>,
) -> PostCompletionOutcome {
    // Exhaustive destructure (no `..`) — the single-home parity lock. Every field
    // is `Copy` (references + scalars), so the `&Struct { .. }` pattern copies
    // each out by value. `iteration`, `default_model`, and `permission_mode` are
    // consumed only by the production review closure / `mutate_prd_from_feedback`
    // in [`react_to_completions`], so they are acknowledged here as `_` rather
    // than elided with `..`.
    let &PostCompletionParams {
        run_id,
        iteration: _,
        working_root,
        prd_file,
        task_prefix,
        default_model: _,
        permission_mode: _,
        external_repo_path,
        external_git_scan_depth,
        wrapper_commit,
    } = params;

    // (1) Wrapper-commit (#8). Sequential only; the slot merge-back already
    //     carries the commit on the wave path.
    let mut wrapper_commit_hash = None;
    if wrapper_commit {
        for id in completed_ids {
            if let Some(hash) =
                git_reconcile::wrapper_commit(working_root, id, "loop wrapper commit")
            {
                wrapper_commit_hash = Some(hash);
            }
        }
    }

    // (2) External-git completion shadow (#9). Empty when no external repo is
    //     configured — the hermetic test path.
    let external_reconciled = match external_repo_path {
        Some(ext_repo) => git_reconcile::reconcile_external_git_completions(
            ext_repo,
            conn,
            run_id,
            prd_file,
            task_prefix,
            external_git_scan_depth as usize,
        ),
        None => Vec::new(),
    };

    // (3) Human review (#10). Input-driven: only ids in the provided set (plus
    //     the external-git ids just discovered) are eligible; a `requires_human`
    //     row absent from the set is never reviewed. Deduped so each id fires at
    //     most once even if it appears in both sources.
    let mut reviewed: HashSet<String> = HashSet::new();
    for id in completed_ids.iter().chain(external_reconciled.iter()) {
        if !reviewed.insert(id.clone()) {
            continue;
        }
        if let Some((title, notes, timeout_secs)) = select_human_review_task(conn, id) {
            review(HumanReviewTask {
                task_id: id.as_str(),
                title: &title,
                notes: notes.as_deref(),
                timeout_secs,
            });
        }
    }

    PostCompletionOutcome {
        external_reconciled,
        wrapper_commit_hash,
    }
}

/// Select a single `requires_human = 1`, `status = 'done'` task by id, returning
/// its `(title, notes, human_review_timeout)` for the [`ReviewFn`] seam. Returns
/// `None` when the id is missing or not a completed `requires_human` task — the
/// input-driven query that replaces the timestamp scan
/// `orchestrator::query_human_review_tasks` used, and `None` on any DB error (a
/// transient read failure simply skips review for that id).
fn select_human_review_task(
    conn: &Connection,
    task_id: &str,
) -> Option<(String, Option<String>, Option<u32>)> {
    conn.query_row(
        "SELECT title, notes, human_review_timeout FROM tasks \
         WHERE id = ?1 AND requires_human = 1 AND status = 'done'",
        [task_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?
                    .and_then(|v| u32::try_from(v).ok()),
            ))
        },
    )
    .optional()
    .ok()
    .flatten()
}
