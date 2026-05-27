//! FEAT-PRIMARY-002 + FEAT-PRIMARY-005 — integration tests for
//! `primaryRunner` routing: the full path from `ProjectConfig` →
//! `resolve_task_model` → `resolve_effective_runner`.
//!
//! These tests verify the routing precedence chain:
//!   explicit task model > primaryRunner match > difficulty=high > prd/project/user defaults
//!
//! Coverage (8 scenarios):
//!   1. `byTaskType "review"` match → Grok model + RunnerKind::Grok
//!   2. `byTaskType "milestone"` match → Grok model + RunnerKind::Grok
//!   3. `byIdPrefix` match when `task_type` is absent → Grok model
//!   4. `byTaskType` wins over `byIdPrefix` when both produce a match
//!   5. Non-matched task (task_type + id not in maps) → falls through to
//!      project_default (no primaryRunner override), resolved to Claude
//!   6. `primaryRunner` absent (`None`) → behavior identical to no-config
//!   7. Explicit `task_model` (rung 1) wins over `primaryRunner` match (rung 2)
//!   8. End-to-end: `resolve_effective_runner` returns `RunnerKind::Grok` for
//!      a primaryRunner-matched task, confirming the model string produced by
//!      `resolve_task_model` feeds the runner-selection correctly.
//!
//! Integration fixture: the config structures are built in-memory to represent
//! the equivalent of a `.task-mgr/config.json` with `primaryRunner` populated,
//! covering both `byTaskType` and `byIdPrefix` maps.

use std::collections::HashMap;

use task_mgr::loop_engine::engine::{IterationContext, resolve_effective_runner};
use task_mgr::loop_engine::model::{
    ModelResolutionContext, OPUS_MODEL, SONNET_MODEL, resolve_task_model,
};
use task_mgr::loop_engine::project_config::{PrimaryRunnerConfig, RunnerSpec};
use task_mgr::loop_engine::runner::RunnerKind;

/// Grok model used throughout these tests — mirrors what a real operator would
/// write in `byTaskType["review"].model`.
const GROK_MODEL: &str = "grok-4-fast";

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a `PrimaryRunnerConfig` with `review` and `milestone` wired to Grok,
/// and `REVIEW-` / `MILESTONE-` id-prefix entries for the byIdPrefix map.
fn make_cfg() -> PrimaryRunnerConfig {
    let grok_spec = || RunnerSpec {
        provider: "grok".to_string(),
        model: GROK_MODEL.to_string(),
    };
    let mut by_task_type = HashMap::new();
    by_task_type.insert("review".to_string(), grok_spec());
    by_task_type.insert("milestone".to_string(), grok_spec());

    let mut by_id_prefix = HashMap::new();
    by_id_prefix.insert("REVIEW-".to_string(), grok_spec());
    by_id_prefix.insert("MILESTONE-".to_string(), grok_spec());

    PrimaryRunnerConfig {
        claude_fallback_model: Some(SONNET_MODEL.to_string()),
        by_task_type,
        by_id_prefix,
        ..Default::default()
    }
}

// ── Scenario 1 — byTaskType "review" match ────────────────────────────────────

/// `task_type = "review"` with a matching `byTaskType["review"]` entry routes
/// to the configured Grok model. This is the canonical path for `CODE-REVIEW-*`
/// and `REVIEW-*` tasks when the operator wants to run them on Grok.
#[test]
fn by_task_type_review_routes_to_grok() {
    let cfg = make_cfg();
    let result = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: Some("review"),
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(GROK_MODEL),
        "byTaskType review match MUST resolve to the configured Grok model — \
         the primaryRunner rung beats project_default",
    );
}

// ── Scenario 2 — byTaskType "milestone" match ─────────────────────────────────

/// `task_type = "milestone"` routes to Grok via `byTaskType["milestone"]`.
/// Verifies the byTaskType map is checked for every configured key, not just
/// the first entry.
#[test]
fn by_task_type_milestone_routes_to_grok() {
    let cfg = make_cfg();
    let result = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-MILESTONE-FINAL"),
        task_type: Some("milestone"),
        primary_runner: Some(&cfg),
        prd_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(GROK_MODEL),
        "byTaskType milestone match MUST resolve to the configured Grok model",
    );
}

// ── Scenario 3 — byIdPrefix match when task_type is absent ───────────────────

/// When `task_type` is `None`, `byTaskType` produces no match and the resolver
/// falls through to `byIdPrefix`. A task ID whose body starts with `"REVIEW-"`
/// MUST still route to Grok.
#[test]
fn by_id_prefix_match_when_task_type_is_absent() {
    let cfg = make_cfg();
    let result = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: None, // no task_type supplied
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(GROK_MODEL),
        "byIdPrefix MUST match 'REVIEW-001' when byTaskType produces no match \
         (task_type absent) — the id-prefix fallback must not be skipped",
    );
}

// ── Scenario 4 — byTaskType wins over byIdPrefix when both match ──────────────

/// When both `byTaskType["review"]` and `byIdPrefix["REVIEW-"]` match,
/// `byTaskType` wins. This guards against a future refactor that accidentally
/// reorders the checks and lets a lower-priority prefix spec win.
///
/// To make this observable, we use different models for the two specs.
#[test]
fn by_task_type_wins_over_by_id_prefix_when_both_match() {
    let grok_prefix_model = "grok-4";
    let mut by_task_type = HashMap::new();
    by_task_type.insert(
        "review".to_string(),
        RunnerSpec {
            provider: "grok".to_string(),
            model: GROK_MODEL.to_string(), // "grok-4-fast"
        },
    );
    let mut by_id_prefix = HashMap::new();
    by_id_prefix.insert(
        "REVIEW-".to_string(),
        RunnerSpec {
            provider: "grok".to_string(),
            model: grok_prefix_model.to_string(), // "grok-4" — different
        },
    );
    let cfg = PrimaryRunnerConfig {
        by_task_type,
        by_id_prefix,
        ..Default::default()
    };

    let result = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: Some("review"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(GROK_MODEL), // "grok-4-fast", NOT "grok-4"
        "byTaskType MUST win over byIdPrefix when both maps contain a matching entry",
    );
}

// ── Scenario 5 — non-matched task falls through to project_default ────────────

/// A FEAT task whose type is absent from `byTaskType` and whose ID has no
/// matching prefix in `byIdPrefix` MUST NOT be routed by `primaryRunner`. The
/// resolution chain falls through to `project_default` (Claude model).
#[test]
fn non_matched_task_falls_through_to_project_default() {
    let cfg = make_cfg(); // has "review" + "milestone" only
    let result = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-FEAT-001"),
        task_type: Some("implementation"),
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(SONNET_MODEL),
        "FEAT task MUST NOT be routed by primaryRunner — the resolution chain \
         falls through to project_default",
    );
}

// ── Scenario 6 — primaryRunner absent → byte-identical to no-config ───────────

/// When `primary_runner = None`, the rung is skipped entirely and the
/// behavior is byte-identical to the pre-primary-runner chain. The same
/// REVIEW-001 task that would be routed to Grok with a `primaryRunner` present
/// resolves to the project_default instead.
#[test]
fn primary_runner_absent_skips_rung_entirely() {
    let result_without = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: Some("review"),
        primary_runner: None, // absent
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    // With primaryRunner present the same task goes to Grok.
    let cfg = make_cfg();
    let result_with = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: Some("review"),
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });

    assert_eq!(
        result_without.as_deref(),
        Some(SONNET_MODEL),
        "absent primaryRunner MUST not route the task — resolution falls to project_default",
    );
    assert_eq!(
        result_with.as_deref(),
        Some(GROK_MODEL),
        "present primaryRunner MUST route the review task to Grok",
    );
    assert_ne!(
        result_without, result_with,
        "the two configs MUST produce different results for the same task id",
    );
}

// ── Scenario 7 — explicit task_model (rung 1) wins over primaryRunner ─────────

/// An explicit `task_model` set on the task beats `primaryRunner` matching
/// (rung 1 > rung 2). This guards against a regression where the primaryRunner
/// check runs before the explicit-model check.
#[test]
fn explicit_task_model_wins_over_primary_runner_match() {
    let cfg = make_cfg();
    let explicit_model = OPUS_MODEL;

    let result = resolve_task_model(&ModelResolutionContext {
        task_model: Some(explicit_model), // rung 1
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: Some("review"),
        primary_runner: Some(&cfg), // would route to Grok without task_model
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(explicit_model),
        "explicit task_model MUST win over primaryRunner match — rung 1 > rung 2",
    );
    assert_ne!(
        result.as_deref(),
        Some(GROK_MODEL),
        "primaryRunner MUST NOT override an explicit task_model",
    );
}

// ── Scenario 8 — end-to-end: resolve_effective_runner returns Grok ────────────

/// Smoke-tests the full composition:
///   resolve_task_model → effective_model string → resolve_effective_runner → RunnerKind
///
/// This confirms that the model string produced by `resolve_task_model` for a
/// primaryRunner-matched task flows into `resolve_effective_runner` and
/// correctly selects `RunnerKind::Grok` via `provider_for_model` token equality.
///
/// Integration fixture: uses a prefixed task ID ("8d71d1f7-REVIEW-001") as a
/// stand-in for a real PRD task, matching the shape of a JSON task list entry
/// with `taskType: "review"`.
#[test]
fn end_to_end_primary_runner_matched_task_resolves_grok_runner() {
    let cfg = make_cfg();
    let ctx = IterationContext::new(8);

    // Step 1: model resolution (what the loop does before spawning).
    let effective_model = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: Some("review"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(
        effective_model.as_deref(),
        Some(GROK_MODEL),
        "resolve_task_model MUST return the Grok model for a review task",
    );

    // Step 2: runner resolution (what the loop's dispatch site does).
    let runner = resolve_effective_runner(&ctx, "8d71d1f7-REVIEW-001", effective_model.as_deref());
    assert_eq!(
        runner,
        RunnerKind::Grok,
        "resolve_effective_runner MUST return RunnerKind::Grok when the effective_model \
         is a Grok model id — provider_for_model token-equality is the single SSoT",
    );

    // Negative: a non-matched task in the same ctx still resolves to Claude.
    let feat_model = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-FEAT-001"),
        task_type: Some("implementation"),
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    let feat_runner = resolve_effective_runner(&ctx, "8d71d1f7-FEAT-001", feat_model.as_deref());
    assert_eq!(
        feat_runner,
        RunnerKind::Claude,
        "non-matched FEAT task MUST still resolve to Claude even when primaryRunner is configured",
    );
}

// ── Scenario 9 — byIdPrefix enforces a dash boundary (no false-positive) ─────

/// A `byIdPrefix` key MUST only match at a dash-delimited segment boundary.
/// `"REVIEW-"` matches `REVIEW-001` but MUST NOT match `REVIEWER-001` — a naive
/// `starts_with` would wrongly route the unrelated `REVIEWER-*` task to Grok.
#[test]
fn by_id_prefix_does_not_false_match_longer_segment() {
    let cfg = make_cfg(); // has byIdPrefix["REVIEW-"]
    let result = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEWER-001"),
        task_type: None,
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        result.as_deref(),
        Some(SONNET_MODEL),
        "key 'REVIEW-' MUST NOT match id body 'REVIEWER-001' — the prefix must \
         end on a '-' boundary, so this task falls through to project_default",
    );
}

// ── Scenario 10 — byIdPrefix key normalization (with/without trailing dash) ──

/// A `byIdPrefix` key written WITHOUT a trailing dash (`"REVIEW"`) behaves
/// identically to `"REVIEW-"`: it matches `REVIEW-001` (dash boundary enforced
/// after normalization) and still rejects `REVIEWER-001`.
#[test]
fn by_id_prefix_key_without_trailing_dash_is_normalized() {
    let grok_spec = RunnerSpec {
        provider: "grok".to_string(),
        model: GROK_MODEL.to_string(),
    };
    let mut by_id_prefix = HashMap::new();
    by_id_prefix.insert("REVIEW".to_string(), grok_spec); // no trailing '-'
    let cfg = PrimaryRunnerConfig {
        by_id_prefix,
        ..Default::default()
    };

    let matched = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEW-001"),
        task_type: None,
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        matched.as_deref(),
        Some(GROK_MODEL),
        "key 'REVIEW' (no trailing dash) MUST still match 'REVIEW-001' after normalization",
    );

    let rejected = resolve_task_model(&ModelResolutionContext {
        task_id: Some("8d71d1f7-REVIEWER-001"),
        task_type: None,
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        rejected.as_deref(),
        Some(SONNET_MODEL),
        "key 'REVIEW' MUST NOT false-match 'REVIEWER-001' — normalization adds the dash boundary",
    );
}
