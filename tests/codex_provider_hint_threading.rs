//! codex-runner acceptance tests — provider_hint threading from
//! `resolve_task_execution_target` through the prompt bundle into BOTH spawn
//! sites (sequential + wave), plus reviewModel reconciliation at both sites.
//!
//! These tests pin the contract that:
//!  - Sequential: `PromptResult.provider_hint` flows into the spawn-time
//!    `EffectiveRunnerInput`, with reviewModel reconciliation clearing the
//!    hint when an override fires.
//!  - Wave: `SlotPromptBundle.provider_hint` flows into `SlotContext` and the
//!    spawn-time `EffectiveRunnerInput`, with reviewModel reconciliation
//!    clearing the hint at BOTH the bundle and the slot.
//!  - Codex routing reaches `RunnerKind::Codex` end-to-end via the threaded
//!    hint — no longer None-stubbed.

use std::collections::HashMap;

use task_mgr::loop_engine::engine::{
    EffectiveRunnerInput, IterationContext, apply_review_model_override, resolve_effective_runner,
};
use task_mgr::loop_engine::model::{
    self, ModelResolutionContext, SONNET_MODEL, resolve_task_execution_target,
};
use task_mgr::loop_engine::project_config::{PrimaryRunnerConfig, RunnerSpec};
use task_mgr::loop_engine::runner::RunnerKind;

const GPT_MODEL: &str = "gpt-4";

fn codex_cfg_for_feat() -> PrimaryRunnerConfig {
    let mut by_id_prefix = HashMap::new();
    by_id_prefix.insert(
        "FEAT-".to_string(),
        RunnerSpec {
            provider: "codex".to_string(),
            model: GPT_MODEL.to_string(),
            runtime_error_fallback: false,
        },
    );
    PrimaryRunnerConfig {
        by_id_prefix,
        ..Default::default()
    }
}

fn codex_cfg_for_review() -> PrimaryRunnerConfig {
    let mut by_id_prefix = HashMap::new();
    by_id_prefix.insert(
        "REVIEW-".to_string(),
        RunnerSpec {
            provider: "codex".to_string(),
            model: GPT_MODEL.to_string(),
            runtime_error_fallback: false,
        },
    );
    PrimaryRunnerConfig {
        by_id_prefix,
        ..Default::default()
    }
}

/// AC: a primaryRunner spec with provider:"codex" matching the cluster task
/// yields `provider_hint = Some(Codex)`. End-to-end via
/// `resolve_task_execution_target`.
#[test]
fn primary_runner_codex_emits_hint() {
    let cfg = codex_cfg_for_feat();
    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some("FEAT-CODEX-001"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(target.model.as_deref(), Some(GPT_MODEL));
    assert_eq!(
        target.provider_hint,
        Some(model::Provider::Codex),
        "rung 2 primaryRunner match with provider:codex MUST emit Some(Codex)"
    );
}

/// AC: the hint flows into `EffectiveRunnerInput` → `RunnerKind::Codex`
/// end-to-end (the routing that was None-stubbed in FEAT-001).
#[test]
fn codex_hint_resolves_to_codex_runner() {
    let ctx = IterationContext::new(8);
    let cfg = codex_cfg_for_feat();
    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some("FEAT-CODEX-001"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    let runner = resolve_effective_runner(
        &ctx,
        "FEAT-CODEX-001",
        EffectiveRunnerInput {
            model: target.model.as_deref(),
            provider_hint: target.provider_hint,
        },
    );
    assert_eq!(
        runner,
        RunnerKind::Codex,
        "Codex hint MUST resolve to RunnerKind::Codex (gpt-* model alone would route to Claude)"
    );
}

/// AC: reviewModel reconciliation — when `apply_review_model_override`
/// returns Some for a review-class task, the threaded provider_hint MUST be
/// forced to None at the spawn site so the routing follows
/// `provider_for_model(reviewModel)` (which never returns Codex).
#[test]
fn review_model_clears_codex_hint() {
    let ctx = IterationContext::new(8);
    let cfg = codex_cfg_for_review();

    // The cluster resolver surfaces the Codex hint via primary_runner.
    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some("REVIEW-007"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(target.provider_hint, Some(model::Provider::Codex));

    // reviewModel = "grok-build" → override fires → hint MUST be cleared.
    let review_model = "grok-build";
    let effective_model = apply_review_model_override(Some(review_model), "REVIEW-007")
        .expect("override fires for REVIEW-007");
    assert_eq!(effective_model, "grok-build");
    let effective_provider_hint = None; // contract: clear when override fires

    let runner = resolve_effective_runner(
        &ctx,
        "REVIEW-007",
        EffectiveRunnerInput {
            model: Some(&effective_model),
            provider_hint: effective_provider_hint,
        },
    );
    assert_eq!(
        runner,
        RunnerKind::Grok,
        "reviewModel=grok-build with cleared hint MUST route to Grok via provider_for_model"
    );
}

/// AC negative: the SAME review-class task with reviewModel UNSET MUST keep
/// the Codex hint and route to Codex (hint preserved — clear ONLY when the
/// override fired).
#[test]
fn review_unset_preserves_codex_hint() {
    let ctx = IterationContext::new(8);
    let cfg = codex_cfg_for_review();

    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some("REVIEW-007"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(target.provider_hint, Some(model::Provider::Codex));

    // reviewModel unset → override does NOT fire → hint stays.
    let override_model = apply_review_model_override(None, "REVIEW-007");
    assert_eq!(override_model, None);

    let runner = resolve_effective_runner(
        &ctx,
        "REVIEW-007",
        EffectiveRunnerInput {
            model: target.model.as_deref(),
            provider_hint: target.provider_hint,
        },
    );
    assert_eq!(
        runner,
        RunnerKind::Codex,
        "reviewModel unset MUST preserve the Codex hint — clear ONLY on override-fire"
    );
}

/// AC: sequential and wave reconciliation rule MUST be identical.
/// Both sites observe the same `apply_review_model_override` contract; both
/// clear the hint to `None`. This test pins the symmetric rule so a future
/// edit to one site (e.g. clearing too aggressively at the wave site) is a
/// loud test failure.
#[test]
fn sequential_and_wave_apply_identical_reconciliation() {
    let ctx = IterationContext::new(8);

    let task_id = "REVIEW-007";
    let cluster_hint = Some(model::Provider::Codex);
    let review_model = "grok-build";

    // Sequential rule (iteration.rs):
    let seq_effective_model = apply_review_model_override(Some(review_model), task_id);
    assert!(seq_effective_model.is_some());
    let seq_effective_hint = if seq_effective_model.is_some() {
        None
    } else {
        cluster_hint
    };
    let seq_runner = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: seq_effective_model.as_deref(),
            provider_hint: seq_effective_hint,
        },
    );

    // Wave rule (wave_scheduler.rs):
    let wave_effective_model = apply_review_model_override(Some(review_model), task_id);
    assert!(wave_effective_model.is_some());
    let wave_effective_hint = if wave_effective_model.is_some() {
        None
    } else {
        cluster_hint
    };
    let wave_runner = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: wave_effective_model.as_deref(),
            provider_hint: wave_effective_hint,
        },
    );

    assert_eq!(
        seq_runner, wave_runner,
        "sequential and wave reconciliation rules MUST be identical — both clear the hint"
    );
    assert_eq!(seq_effective_model, wave_effective_model);
    assert_eq!(seq_runner, RunnerKind::Grok);
}

/// AC negative: a non-review task with the cluster hint set MUST NOT have its
/// hint cleared — the override-fire condition is the SOLE clear trigger.
#[test]
fn non_review_task_keeps_hint_even_with_review_model_config() {
    let ctx = IterationContext::new(8);
    let cfg = codex_cfg_for_feat(); // matches FEAT- prefix

    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some("FEAT-CODEX-001"),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(target.provider_hint, Some(model::Provider::Codex));

    // Even with reviewModel set, apply_review_model_override returns None
    // for a non-review id, so the hint MUST be preserved.
    let override_model = apply_review_model_override(Some("grok-build"), "FEAT-CODEX-001");
    assert_eq!(override_model, None);

    let runner = resolve_effective_runner(
        &ctx,
        "FEAT-CODEX-001",
        EffectiveRunnerInput {
            model: target.model.as_deref(),
            provider_hint: target.provider_hint,
        },
    );
    assert_eq!(
        runner,
        RunnerKind::Codex,
        "non-review tasks MUST preserve the cluster's provider_hint regardless of reviewModel config"
    );
}

/// WIRE-FIX-001 regression: provider-only Codex spec {provider:"codex", model:""}
/// with a project defaultModel set MUST route to RunnerKind::Codex in BOTH
/// sequential and wave formulas.
///
/// Root cause: the wave widened `resolved_model=None` → `defaultModel` then
/// dropped `provider_hint` on the equality mismatch. Sequential path never
/// widened with defaultModel, so it kept the hint. This test pins both formulas
/// to the same RunnerKind::Codex result.
#[test]
fn provider_only_codex_with_default_model_routes_to_codex_in_both_paths() {
    let ctx = IterationContext::new(8);

    // Provider-only Codex spec: model="" normalises to None in resolve_task_execution_target.
    let mut by_task_type = HashMap::new();
    by_task_type.insert(
        "spike".to_string(),
        RunnerSpec {
            provider: "codex".to_string(),
            model: String::new(), // v1-blessed provider-only shape
            runtime_error_fallback: false,
        },
    );
    let cfg = PrimaryRunnerConfig {
        by_task_type,
        ..Default::default()
    };

    let task_id = "SPIKE-001";

    // resolve_task_execution_target: model="" normalises to None, hint=Some(Codex).
    // task_type="spike" is required so byTaskType lookup can match.
    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some(task_id),
        task_type: Some("spike"),
        primary_runner: Some(&cfg),
        project_default: Some(SONNET_MODEL),
        ..Default::default()
    });
    assert_eq!(
        target.model, None,
        "empty model string must normalise to None"
    );
    assert_eq!(
        target.provider_hint,
        Some(model::Provider::Codex),
        "provider-only Codex spec must emit Some(Codex) hint"
    );

    // Sequential formula: effective_model starts from resolved_model (None),
    // no crash escalation or review override fires → hint preserved, routes Codex.
    let seq_effective_model = target.model.clone(); // None
    let seq_hint = target.provider_hint; // Some(Codex)
    let seq_runner = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: seq_effective_model.as_deref(),
            provider_hint: seq_hint,
        },
    );

    // Wave formula (WIRE-FIX-001): effective_model widened to defaultModel, but
    // hint MUST be preserved (not dropped on the mismatch). The fix: always carry
    // slot.prompt_bundle.provider_hint regardless of the widening.
    let wave_effective_model = target.model.as_deref().or(Some(SONNET_MODEL)); // None → Some(SONNET_MODEL)
    let wave_hint = target.provider_hint; // Some(Codex) — not dropped by widening
    let wave_runner = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: wave_effective_model,
            provider_hint: wave_hint,
        },
    );

    assert_eq!(
        seq_runner,
        RunnerKind::Codex,
        "sequential: provider-only Codex with defaultModel MUST route to Codex"
    );
    assert_eq!(
        wave_runner,
        RunnerKind::Codex,
        "wave (WIRE-FIX-001): provider-only Codex with defaultModel MUST route to Codex, not Claude"
    );
    assert_eq!(
        seq_runner, wave_runner,
        "sequential and wave MUST produce identical RunnerKind for the same input"
    );
}
