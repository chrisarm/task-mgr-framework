//! FEAT-002: integration + wiring tests for review-class model routing.
//!
//! Pins the contract that `ProjectConfig.review_model` is applied at BOTH
//! dispatch sites (sequential `run_iteration` + wave `run_wave_iteration`)
//! and that the override is:
//! - applied AFTER the crash / overflow escalation block (sequential),
//! - applied by mutating `slot.prompt_bundle.resolved_model` (wave) so the
//!   runner-selection, `--model` flag, and prompt-baked model stay
//!   consistent — a drift `assert!` between `slot_result.effective_runner`
//!   and a re-derivation would otherwise panic.
//!
//! Tests are split between:
//! - **Pure-function checks**: directly call `apply_review_model_override`
//!   against varied task ids and `reviewModel` values. End-to-end shape via
//!   `resolve_effective_runner` confirms the override + runner-selection
//!   composition picks the right `RunnerKind`.
//! - **Bundle-mutation checks**: build a `SlotPromptBundle` and run the
//!   exact override step the wave loop does, asserting the bundle's
//!   `resolved_model` field is rewritten (not just a transient local).
//! - **Source-grep wiring**: read `src/loop_engine/engine.rs` and assert
//!   the override is wired at both call sites in the right relative order.
//!   This catches reverts/refactors that move the override before
//!   escalation (sequential) or that fork the predicate.

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::engine::{
    IterationContext, apply_review_model_override, resolve_effective_runner,
};
use task_mgr::loop_engine::model::OPUS_MODEL;
use task_mgr::loop_engine::prompt::slot::{SlotPromptParams, build_prompt};
use task_mgr::loop_engine::runner::RunnerKind;
use task_mgr::models::Task;

// ── End-to-end shape via apply_review_model_override + resolve_effective_runner ──

/// AC: positive (sequential shape). With `reviewModel = "grok-build"`, a
/// review-class task (`CODE-REVIEW-1`, `MILESTONE-FINAL`, `REVIEW-001`)
/// resolves `effective_model` to `grok-build` and `resolve_effective_runner`
/// returns `RunnerKind::Grok`. The `--model` flag downstream would receive
/// the same string — this composition is what the sequential dispatch site
/// does.
#[test]
fn sequential_review_class_routes_to_grok_build() {
    let ctx = IterationContext::new(8);
    // Use prefixed ids — production task ids carry an 8-hex-char prefix.
    for task_id in &[
        "8d71d1f7-CODE-REVIEW-1",
        "8d71d1f7-MILESTONE-FINAL",
        "8d71d1f7-REVIEW-001",
    ] {
        let effective_model = apply_review_model_override(Some("grok-build"), task_id);
        assert_eq!(
            effective_model.as_deref(),
            Some("grok-build"),
            "review-class id {task_id} MUST resolve effective_model to grok-build \
             when reviewModel is grok-build — same string flows to the `--model` flag",
        );
        assert_eq!(
            resolve_effective_runner(&ctx, task_id, effective_model.as_deref()),
            RunnerKind::Grok,
            "review-class id {task_id} resolves to RunnerKind::Grok via \
             provider_for_model token-equality",
        );
    }
}

/// AC: negative — `reviewModel` unset → no change at the sequential site.
/// The baked-in model (typically Opus) survives and the runner stays Claude.
#[test]
fn sequential_review_model_unset_leaves_review_task_on_baked_model() {
    let ctx = IterationContext::new(8);
    let task_id = "8d71d1f7-CODE-REVIEW-1";
    // No override → caller's baked-in model (Opus) flows through unchanged.
    let override_model = apply_review_model_override(None, task_id);
    assert_eq!(
        override_model, None,
        "unset reviewModel MUST NOT override the review-class task's baked model",
    );
    let effective_model = override_model.or(Some(OPUS_MODEL.to_string()));
    assert_eq!(
        resolve_effective_runner(&ctx, task_id, effective_model.as_deref()),
        RunnerKind::Claude,
        "without reviewModel, review-class tasks stay on Claude (Opus by default)",
    );
}

/// AC: negative — non-review tasks (`VERIFY-001`, `MILESTONE-1`,
/// `REFACTOR-001`, `REFACTOR-REVIEW-FINAL`) are NOT rerouted even when
/// `reviewModel` is configured. This is the discriminator that proves the
/// predicate is review-class-specific, not "any task with reviewModel set."
#[test]
fn non_review_tasks_are_not_routed_when_review_model_is_set() {
    let ctx = IterationContext::new(8);
    for task_id in &[
        "8d71d1f7-VERIFY-001",
        "8d71d1f7-MILESTONE-1",
        "8d71d1f7-MILESTONE-2",
        "8d71d1f7-REFACTOR-001",
        "8d71d1f7-REFACTOR-REVIEW-FINAL",
        "8d71d1f7-FEAT-001",
    ] {
        assert_eq!(
            apply_review_model_override(Some("grok-build"), task_id),
            None,
            "non-review id {task_id} MUST NOT receive reviewModel routing — \
             the predicate must be review-class-specific",
        );
        // And the baked Opus survives → Claude runner.
        let effective_model = apply_review_model_override(Some("grok-build"), task_id)
            .or(Some(OPUS_MODEL.to_string()));
        assert_eq!(
            resolve_effective_runner(&ctx, task_id, effective_model.as_deref()),
            RunnerKind::Claude,
            "non-review {task_id} stays on Claude runner",
        );
    }
}

// ── Wave-path bundle mutation: rewriting `prompt_bundle.resolved_model` ──────

fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().unwrap();
    let mut conn = open_connection(temp.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp, conn)
}

fn write_base_prompt(temp: &TempDir) -> std::path::PathBuf {
    let p = temp.path().join("prompt.md");
    std::fs::write(&p, "# Base prompt\n").unwrap();
    p
}

/// AC: positive (wave). With `reviewModel = "grok-build"`, the per-slot
/// `prompt_bundle.resolved_model` for a review-class slot is rewritten to
/// `grok-build` BEFORE the slot worker spawns. This mirrors the wave loop's
/// inner per-slot mutation step — the field, not a transient local, is the
/// thing that feeds runner selection, the `--model` flag, and the
/// prompt-baked model.
///
/// **Known-bad guard**: this assertion catches the failure mode where a
/// patch overrides only a local `effective_model` and leaves
/// `prompt_bundle.resolved_model` as the baked Opus constant — runner would select
/// Grok but the `--model` flag would still pass the Claude model id.
#[test]
fn wave_review_class_slot_bundle_resolved_model_is_rewritten_to_grok_build() {
    let (temp, conn) = setup_migrated_db();
    let base = write_base_prompt(&temp);

    // Insert a review-class task into the DB so `build_prompt` can resolve
    // the persisted task row.
    let task_id = "8d71d1f7-CODE-REVIEW-1";
    let mut task = Task::new(task_id, "Review the implementation");
    task.acceptance_criteria = vec!["Code is correct".to_string()];
    task.difficulty = Some("high".to_string());
    task.model = Some(OPUS_MODEL.to_string());
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, difficulty, priority, max_retries) \
         VALUES (?, ?, 'in_progress', ?, ?, 0, 5)",
        rusqlite::params![&task.id, &task.title, &task.model, &task.difficulty],
    )
    .unwrap();

    // Build the slot bundle the way the wave path does.
    let params = SlotPromptParams {
        project_root: temp.path().to_path_buf(),
        base_prompt_path: base.clone(),
        permission_mode: PermissionMode::text_only(),
        steering_path: None,
        session_guidance: "",
    };
    let bundle = build_prompt(&conn, &task, &params);

    // Pre-condition: the bundle was built with the task's baked Opus model.
    assert_eq!(
        bundle.resolved_model.as_deref(),
        Some(OPUS_MODEL),
        "pre-condition: bundle.resolved_model starts at the task's baked Opus",
    );

    // Mirror the wave-loop's per-slot mutation step. Production code at
    // `run_wave_iteration` does exactly this assignment after the bundle is
    // built and before the runner resolves.
    let mut bundle = bundle;
    if let Some(rm) = apply_review_model_override(Some("grok-build"), &bundle.task_id) {
        bundle.resolved_model = Some(rm);
    }

    // The bundle field (not a transient local) carries the rewritten model
    // — this is what `run_slot_iteration` reads for the `--model` flag.
    assert_eq!(
        bundle.resolved_model.as_deref(),
        Some("grok-build"),
        "the bundle's resolved_model field MUST be rewritten — overriding \
         only a local would send the Grok runner a Claude `--model` value",
    );

    // And resolving the runner from this same string picks Grok.
    let ctx = IterationContext::new(8);
    assert_eq!(
        resolve_effective_runner(&ctx, &bundle.task_id, bundle.resolved_model.as_deref()),
        RunnerKind::Grok,
        "with the bundle rewritten, the wave loop's resolve_effective_runner \
         step selects RunnerKind::Grok",
    );
}

/// Negative wave: a non-review task in the same wave does NOT get its
/// bundle rewritten, even when reviewModel is set.
#[test]
fn wave_non_review_slot_bundle_is_untouched_when_review_model_is_set() {
    let (temp, conn) = setup_migrated_db();
    let base = write_base_prompt(&temp);

    let task_id = "8d71d1f7-FEAT-001";
    let mut task = Task::new(task_id, "Implement feature");
    task.acceptance_criteria = vec!["Feature works".to_string()];
    task.model = Some(OPUS_MODEL.to_string());
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, priority, max_retries) \
         VALUES (?, ?, 'in_progress', ?, 0, 5)",
        rusqlite::params![&task.id, &task.title, &task.model],
    )
    .unwrap();

    let params = SlotPromptParams {
        project_root: temp.path().to_path_buf(),
        base_prompt_path: base.clone(),
        permission_mode: PermissionMode::text_only(),
        steering_path: None,
        session_guidance: "",
    };
    let mut bundle = build_prompt(&conn, &task, &params);

    // Mirror the wave-loop's override step.
    if let Some(rm) = apply_review_model_override(Some("grok-build"), &bundle.task_id) {
        bundle.resolved_model = Some(rm);
    }

    // Non-review id → no change at all.
    assert_eq!(
        bundle.resolved_model.as_deref(),
        Some(OPUS_MODEL),
        "non-review FEAT-001 MUST NOT have its bundle rewritten — even with \
         reviewModel configured, the override is review-class-specific",
    );
}

// ── Source-grep wiring: both dispatch sites read `params.project_config.review_model` ──

/// Source-grep: `run_iteration` MUST call `apply_review_model_override`
/// with the in-scope `params.project_config.review_model`, and the result
/// MUST flow into `effective_model` AFTER the crash/overflow escalation
/// block (so escalation can't overwrite the routing) and BEFORE
/// `resolve_effective_runner`.
///
/// Known-bad guard #1: a refactor placing the override BEFORE
/// `ctx.model_overrides` consultation lets a stale prior-overflow override
/// shadow the routing. The relative-order assert below catches that.
///
/// Known-bad guard #2: a re-read of `read_project_config(...)` here would
/// bypass the in-scope reference — the grep on `params.project_config` is
/// the canonical access pattern.
#[test]
fn run_iteration_wires_review_model_override_at_the_right_spot() {
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

    // Wiring check #1: helper is called.
    assert!(
        body.contains("apply_review_model_override("),
        "run_iteration MUST call apply_review_model_override — the shared \
         predicate is the single source of truth for the routing check",
    );

    // Wiring check #2: the in-scope config reference is the input.
    assert!(
        body.contains("params.project_config.review_model"),
        "run_iteration MUST read `params.project_config.review_model` — the \
         reference is already passed in; re-reading config from disk is wrong",
    );

    // Wiring check #3: the result mutates `effective_model` (not a new local
    // that never reaches `resolve_effective_runner`).
    assert!(
        body.contains("effective_model = Some("),
        "run_iteration MUST assign the override into `effective_model` so \
         both runner selection and the --model flag see the new value",
    );

    // Relative-order check: the override site MUST be AFTER both
    // `check_crash_escalation` and `ctx.model_overrides`, and BEFORE
    // `resolve_effective_runner`. Otherwise escalation can overwrite the
    // routing (known-bad path called out in the AC).
    let override_idx = body
        .find("apply_review_model_override(")
        .expect("expected apply_review_model_override call in run_iteration body");
    let crash_escalation_idx = body
        .find("check_crash_escalation(")
        .expect("expected check_crash_escalation call in run_iteration body");
    let model_overrides_idx = body
        .find("ctx.model_overrides.get(&task_id)")
        .expect("expected ctx.model_overrides.get(&task_id) in run_iteration body");
    let resolve_idx = body
        .find("resolve_effective_runner(")
        .expect("expected resolve_effective_runner call in run_iteration body");

    assert!(
        override_idx > crash_escalation_idx,
        "review-model override MUST be applied AFTER crash escalation — \
         otherwise crash escalation could overwrite the routing",
    );
    assert!(
        override_idx > model_overrides_idx,
        "review-model override MUST be applied AFTER the overflow \
         model_overrides consultation — otherwise overflow recovery could \
         overwrite the routing",
    );
    assert!(
        override_idx < resolve_idx,
        "review-model override MUST be applied BEFORE resolve_effective_runner \
         so the runner selection sees the rewritten model",
    );
}

/// Source-grep: `run_wave_iteration` MUST call
/// `apply_review_model_override` and mutate `slot.prompt_bundle.resolved_model`
/// before `resolve_effective_runner` recomputes for the slot. Anything else
/// (mutating a transient local, putting it in `runner_overrides`) sends a
/// Claude `--model` to the Grok runner or trips the drift sentinel.
#[test]
fn run_wave_iteration_wires_review_model_override_into_bundle() {
    // `run_wave_iteration` was carved into `wave_scheduler.rs` (PRD 02,
    // FEAT-003); the source-grep follows it there.
    let source = std::fs::read_to_string("src/loop_engine/wave_scheduler.rs")
        .expect("could not read src/loop_engine/wave_scheduler.rs from tests/ cwd");

    let start = source
        .find("pub fn run_wave_iteration(")
        .expect("expected `pub fn run_wave_iteration(` to be defined in wave_scheduler.rs");
    let after_open = &source[start..];
    // `run_wave_iteration` is the last top-level fn in `wave_scheduler.rs`, so
    // the body ends at the `#[cfg(test)]` test module rather than another fn.
    let body_end_rel = ["\nfn ", "\npub fn ", "\npub(crate) fn ", "\n#[cfg(test)]"]
        .iter()
        .filter_map(|marker| {
            after_open[marker.len()..]
                .find(marker)
                .map(|p| p + marker.len())
        })
        .min()
        .expect("expected a top-level fn or test module after run_wave_iteration");
    let body = &after_open[..body_end_rel];

    assert!(
        body.contains("apply_review_model_override("),
        "run_wave_iteration MUST call apply_review_model_override — the \
         shared predicate is the single source of truth for review routing",
    );
    assert!(
        body.contains("params.project_config.review_model"),
        "run_wave_iteration MUST read `params.project_config.review_model`",
    );
    // CRITICAL: mutation lands on the bundle field (not a transient local).
    // The bundle's resolved_model is what `run_slot_iteration` reads for the
    // `--model` flag — overriding only a local would trip the drift sentinel.
    assert!(
        body.contains("slot.prompt_bundle.resolved_model = Some("),
        "run_wave_iteration MUST assign the override into \
         `slot.prompt_bundle.resolved_model` — only the bundle field reaches \
         both the runner selection and the slot worker's --model flag",
    );

    // The override MUST be applied BEFORE the per-slot
    // `resolve_effective_runner` call so runner selection sees the rewrite.
    let override_idx = body
        .find("apply_review_model_override(")
        .expect("expected apply_review_model_override call in run_wave_iteration body");
    let resolve_idx = body
        .find("resolve_effective_runner(")
        .expect("expected resolve_effective_runner call in run_wave_iteration body");
    assert!(
        override_idx < resolve_idx,
        "wave review-model override MUST be applied BEFORE \
         resolve_effective_runner so the runner selection sees the rewrite",
    );
}
