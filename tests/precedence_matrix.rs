//! TEST-010 — end-to-end FR-003 precedence matrix, exercised through the REAL
//! prompt builder (`prompt::slot::build_prompt`), not a direct
//! `resolve_execution_plan` unit call.
//!
//! This file is the semantic re-cover for the two test files REFACTOR-005
//! deleted as premise-dead under the provider-first redesign:
//!
//!   * `tests/primary_runner_routing.rs` — pinned the LEGACY precedence chain
//!     (`explicit model > primaryRunner match > difficulty=high → Opus >
//!     prd/project/user defaults`) through the deleted `resolve_task_model`.
//!     The new precedence chain is `explicit model > routing.byIdPrefix >
//!     task class > quota blackout reroute > anchor window`, resolved by the
//!     single `model::resolve_execution_plan` both prompt builders call. Every
//!     rung is re-covered below as "this rung beats the lower rung".
//!   * `tests/codex_provider_hint_threading.rs` — pinned that a Codex route
//!     threads `provider_hint = Some(Codex)` from resolution through the prompt
//!     bundle into `resolve_effective_runner` → `RunnerKind::Codex` (a `None` /
//!     `gpt-*` model alone routes to Claude). Re-covered by
//!     `codex_byidprefix_route_threads_provider_hint_to_codex_runner`.
//!
//! ## What "through the real prompt builder" buys over a unit call
//!
//! `slot::build_prompt` resolves via `resolve_models_config` +
//! `resolve_execution_plan` exactly as the engine does at spawn time, then maps
//! `ExecutionPlan.provider` to the `SlotPromptBundle.provider_hint`
//! (`Claude → None`, else `Some(provider)`) and surfaces the concrete model in
//! `resolved_model`. Asserting on the bundle pins the WHOLE spawn-side contract,
//! not just the resolver's return value.
//!
//! All configs are production-shaped FR-001 JSON deserialized via serde — never
//! hand-built struct maps (prohibited outcome). Claude model ids come from the
//! `model.rs` constants interpolated into `serde_json::json!`, never literal
//! strings (no_hardcoded_models discipline). Assertions are `assert_eq!` on
//! exact model-id strings, never `contains()`.

use std::collections::HashSet;
use std::fs;

use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::engine::{
    EffectiveRunnerInput, IterationContext, resolve_effective_runner,
};
use task_mgr::loop_engine::model::{FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, Provider, SONNET_MODEL};
use task_mgr::loop_engine::project_config::{ModelsConfig, RoutingConfig};
use task_mgr::loop_engine::prompt::slot::{SlotPromptBundle, SlotPromptParams, build_prompt};
use task_mgr::loop_engine::runner::RunnerKind;
use task_mgr::models::Task;

/// The grok CLI's only model id (the builtin Grok ladder's single rung). Not a
/// Claude id, so the no_hardcoded_models guard (which matches `claude-*` only)
/// does not flag this literal — mirrors `overflow_fallback_rung.rs`.
const GROK_MODEL: &str = "grok-build";

// ── Fixtures ────────────────────────────────────────────────────────────────

fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// A project root with a `prompt.md` so the base-prompt section has content.
fn project_root() -> TempDir {
    let temp = TempDir::new().expect("project tempdir");
    fs::write(temp.path().join("prompt.md"), "# base\n").expect("write base prompt");
    temp
}

/// The shared provider-first `models` block for the matrix: all three providers
/// enabled, `anchor = standard`. Claude carries the full ladder; Grok carries
/// its single `grok-build` rung; Codex carries a provider-only `standard` rung
/// (null model → spawn with no `-m` flag). Routing is supplied per-test.
fn full_models() -> ModelsConfig {
    serde_json::from_value(json!({
        "primaryProvider": "claude",
        "anchor": "standard",
        "providers": {
            "claude": {
                "enabled": true,
                // Configured tier-preserving fallback: the blackout reroute
                // prefers this enabled, non-blacked-out target over the
                // alphabetical "first enabled provider" scan (which would
                // otherwise pick Codex, whose null model makes the reroute
                // target ambiguous). Only consulted on the default-path blackout
                // reroute — inert for every non-blackout rung below.
                "fallback": "grok",
                "tiers": {
                    "cheapest": HAIKU_MODEL,
                    "cost-efficient": SONNET_MODEL,
                    "standard": OPUS_MODEL,
                    "frontier": FABLE_MODEL
                }
            },
            "grok": {
                "enabled": true,
                "tiers": { "standard": GROK_MODEL }
            },
            "codex": {
                "enabled": true,
                "tiers": { "standard": null }
            }
        }
    }))
    .expect("full_models deserializes from FR-001 JSON")
}

fn routing_from(value: serde_json::Value) -> RoutingConfig {
    serde_json::from_value(value).expect("routing deserializes from FR-001 JSON")
}

fn task(id: &str, difficulty: &str, model: Option<&str>) -> Task {
    let mut t = Task::new(id, "precedence matrix fixture");
    t.difficulty = Some(difficulty.to_string());
    t.model = model.map(str::to_string);
    t
}

/// Resolve a task through the REAL slot prompt builder and return the
/// `(resolved_model, provider_hint)` the spawn site would consume.
fn resolve_via_builder(
    conn: &Connection,
    models: &ModelsConfig,
    routing: &RoutingConfig,
    task: &Task,
    blackouts: HashSet<Provider>,
) -> (Option<String>, Option<Provider>) {
    let project = project_root();
    let params = SlotPromptParams {
        project_root: project.path().to_path_buf(),
        base_prompt_path: project.path().join("prompt.md"),
        permission_mode: PermissionMode::Dangerous,
        steering_path: None,
        session_guidance: "",
        primary_runner: None,
        prd_default: None,
        project_default: None,
        user_default: None,
        models_config: models,
        routing_config: routing,
        provider_blackouts: blackouts,
    };
    let bundle: SlotPromptBundle = build_prompt(conn, task, &params);
    assert_eq!(bundle.task_id, task.id, "bundle must mirror the task id");
    (bundle.resolved_model, bundle.provider_hint)
}

// ════════════════════════════════════════════════════════════════════════════
// Rung EXPLICIT_MODEL beats rung BY_ID_PREFIX.
// ════════════════════════════════════════════════════════════════════════════

/// A task with an explicit `tasks.model` resolves to THAT model even when a
/// `routing.byIdPrefix` route would have forced a different provider. (Legacy
/// equivalent: `primary_runner_routing::explicit_task_model_wins_over_primary_runner_match`.)
#[test]
fn explicit_model_beats_byidprefix_route() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    // byIdPrefix would route every FEAT task to Grok...
    let routing = routing_from(json!({ "byIdPrefix": { "FEAT": { "provider": "grok" } } }));

    // ...but the task carries an explicit Claude HAIKU model.
    let t = task("FEAT-001", "medium", Some(HAIKU_MODEL));
    let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, HashSet::new());

    assert_eq!(
        model.as_deref(),
        Some(HAIKU_MODEL),
        "explicit tasks.model wins over the byIdPrefix grok route (rung EXPLICIT_MODEL)",
    );
    assert_eq!(
        hint, None,
        "an explicit Claude model carries no provider hint — the grok route is not consulted",
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Rung BY_ID_PREFIX beats rung TASK_CLASS (the built-in review→frontier force).
// ════════════════════════════════════════════════════════════════════════════

/// Baseline: a REVIEW-class task with NO byIdPrefix route is forced to the
/// frontier tier by the built-in review class → FABLE on Claude.
#[test]
fn review_class_forces_frontier_when_no_route_overrides() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    let routing = routing_from(json!({}));

    let t = task("REVIEW-001", "medium", None);
    let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, HashSet::new());

    assert_eq!(
        model.as_deref(),
        Some(FABLE_MODEL),
        "the built-in review class forces the frontier tier (FABLE) regardless of difficulty",
    );
    assert_eq!(hint, None, "frontier on Claude carries no provider hint");
}

/// A `routing.byIdPrefix` route on a REVIEW id beats the built-in
/// review→frontier force: the route runs at rung BY_ID_PREFIX, BEFORE the class
/// rung. The REVIEW task lands on the route's grok provider at the anchor window
/// (medium → standard → grok-build), NOT frontier FABLE.
#[test]
fn byidprefix_route_beats_review_class_force() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    let routing = routing_from(json!({ "byIdPrefix": { "REVIEW": { "provider": "grok" } } }));

    let t = task("REVIEW-001", "medium", None);
    let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, HashSet::new());

    assert_eq!(
        model.as_deref(),
        Some(GROK_MODEL),
        "the byIdPrefix grok route (rung BY_ID_PREFIX) beats the built-in review→frontier force \
         (rung TASK_CLASS); the route's anchor-window tier (standard) resolves grok-build",
    );
    assert_eq!(
        hint,
        Some(Provider::Grok),
        "the routed Grok provider threads a Grok provider hint to the spawn site",
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Rung TASK_CLASS beats rung QUOTA_BLACKOUT.
//
// A class-FORCED route resolves through `finalize_plan` directly — it never
// enters the default-path blackout reroute. So a planning-class task pinned to
// Claude stays on Claude even under an active Claude blackout.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn class_route_is_immune_to_quota_blackout() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    // Planning class pins to Claude; spillover is enabled so the ONLY reason the
    // task could move to Grok would be the (prohibited-here) blackout reroute.
    let routing = routing_from(json!({
        "taskClasses": { "planning": { "providerPreference": ["claude"] } },
        "spillover": { "maxDifficulty": "high" }
    }));

    let mut blackout = HashSet::new();
    blackout.insert(Provider::Claude);

    let t = task("SPIKE-001", "medium", None);
    let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, blackout);

    assert_eq!(
        model.as_deref(),
        Some(OPUS_MODEL),
        "a class-FORCED Claude route (rung TASK_CLASS) bypasses the blackout reroute entirely; \
         it stays on Claude at the anchor window (medium → standard → OPUS)",
    );
    assert_eq!(
        hint, None,
        "the class-forced Claude provider carries no hint and is never rerouted",
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Rung QUOTA_BLACKOUT beats rung ANCHOR_WINDOW.
//
// A default-path (no explicit / no route / no class force) spillover-eligible
// task reroutes off the blacked-out primary provider; without the blackout it
// would resolve to the primary on the anchor window.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn blackout_reroute_beats_anchor_window_default_path() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    let routing = routing_from(json!({ "spillover": { "maxDifficulty": "high" } }));

    // No blackout → the anchor window picks the primary (Claude) at standard.
    let t = task("FEAT-DEFAULT-001", "medium", None);
    let (baseline_model, baseline_hint) =
        resolve_via_builder(&conn, &models, &routing, &t, HashSet::new());
    assert_eq!(
        baseline_model.as_deref(),
        Some(OPUS_MODEL),
        "without a blackout the default path resolves the primary (Claude) at the anchor window",
    );
    assert_eq!(baseline_hint, None);

    // Claude blacked out → the spillover-eligible medium task reroutes to Grok.
    let mut blackout = HashSet::new();
    blackout.insert(Provider::Claude);
    let (rerouted_model, rerouted_hint) =
        resolve_via_builder(&conn, &models, &routing, &t, blackout);
    assert_eq!(
        rerouted_model.as_deref(),
        Some(GROK_MODEL),
        "rung QUOTA_BLACKOUT reroutes the spillover-eligible default-path task off the \
         blacked-out primary to Grok, beating the anchor window's Claude choice",
    );
    assert_eq!(
        rerouted_hint,
        Some(Provider::Grok),
        "the rerouted Grok provider threads a Grok hint",
    );
}

/// Frontier-critical review tasks are NEVER spillover-eligible: a REVIEW task
/// under a Claude blackout does NOT reroute (it defers instead). Pins the
/// asymmetry the blackout rung depends on (review waits for quota; medium impl
/// spills). The companion deferral assertion lives in
/// `model_selection_engine_edges::blackout_medium_reroutes_while_frontier_review_defers`.
#[test]
fn review_class_does_not_reroute_under_blackout() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    let routing = routing_from(json!({ "spillover": { "maxDifficulty": "high" } }));

    let mut blackout = HashSet::new();
    blackout.insert(Provider::Claude);

    let t = task("REVIEW-002", "medium", None);
    let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, blackout);

    assert_eq!(
        model.as_deref(),
        Some(FABLE_MODEL),
        "a frontier review task is NOT spillover-eligible — it stays on Claude/frontier under a \
         blackout (it defers for quota rather than dropping to another provider)",
    );
    assert_eq!(hint, None, "no reroute → no Grok hint for the review task");
}

// ════════════════════════════════════════════════════════════════════════════
// Rung ANCHOR_WINDOW — the default tier as the difficulty offset over the anchor.
// ════════════════════════════════════════════════════════════════════════════

/// With no explicit model, no route, and no blackout, the anchor window is the
/// sole driver: `low → anchor−1 (cost-efficient → SONNET)`,
/// `medium → anchor (standard → OPUS)`, `high → anchor+1 (frontier → FABLE)`.
/// Replaces the legacy `difficulty=high → Opus` hardcode the deleted
/// `primary_runner_routing.rs` encoded.
#[test]
fn anchor_window_difficulty_offsets_resolve_each_tier() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    let routing = routing_from(json!({}));

    for (difficulty, expected) in [
        ("low", SONNET_MODEL),
        ("medium", OPUS_MODEL),
        ("high", FABLE_MODEL),
    ] {
        let t = task("FEAT-ANCHOR-001", difficulty, None);
        let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, HashSet::new());
        assert_eq!(
            model.as_deref(),
            Some(expected),
            "difficulty={difficulty} must resolve via the anchor window to {expected}",
        );
        assert_eq!(
            hint, None,
            "anchor-window Claude resolution carries no hint"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Codex provider-hint threading (re-covers tests/codex_provider_hint_threading.rs).
//
// A Codex byIdPrefix route resolves to `provider = Codex` with NO model
// (provider-only), threads `provider_hint = Some(Codex)` through the bundle,
// and `resolve_effective_runner` then returns `RunnerKind::Codex`. A None model
// alone would route to Claude — the hint is the only thing that reaches Codex.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn codex_byidprefix_route_threads_provider_hint_to_codex_runner() {
    let (_tmp, conn) = setup_migrated_db();
    let models = full_models();
    let routing = routing_from(json!({ "byIdPrefix": { "FEAT": { "provider": "codex" } } }));

    let t = task("FEAT-CODEX-001", "medium", None);
    let (model, hint) = resolve_via_builder(&conn, &models, &routing, &t, HashSet::new());

    assert_eq!(
        model, None,
        "the Codex standard rung is provider-only (null model) → spawn with no -m flag",
    );
    assert_eq!(
        hint,
        Some(Provider::Codex),
        "a Codex route threads an explicit Codex provider hint (Codex is never inferred from a \
         model string)",
    );

    // End-to-end: the threaded hint, not the (None) model, reaches the runner.
    let ctx = IterationContext::new(5);
    let runner = resolve_effective_runner(
        &ctx,
        &t.id,
        EffectiveRunnerInput {
            model: model.as_deref(),
            provider_hint: hint,
        },
    );
    assert_eq!(
        runner,
        RunnerKind::Codex,
        "the Codex provider hint MUST resolve to RunnerKind::Codex; a None model alone routes to \
         Claude",
    );
}
