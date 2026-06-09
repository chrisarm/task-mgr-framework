//! Engine-integration edge-case suite for the Model-Selection Redesign PRD
//! (TEST-INIT-002). TDD scaffolding for the four engine-level *Known Edge
//! Cases*, written against the CONTRACT-001 foundation types and the EXISTING
//! engine functions they build on:
//!
//!   * `invalidate_stale_overrides` (operator escape valve, `reactions::pre_spawn`)
//!   * `CapabilityTier` / `anchored_tier` / `resolve_models_config` /
//!     `ResolvedModelsConfig::effort_for` (CONTRACT-001, `loop_engine::model`)
//!   * `detect_legacy_model_keys` / `validate_models_config` /
//!     `read_project_config` (CONTRACT-001, `loop_engine::project_config`)
//!   * `IterationContext::stale_tracker` (`loop_engine::stale::StaleTracker`)
//!
//! ## Map of the four edge cases → owning FEAT
//!
//! | edgeCases[] | scenario                                   | owner    |
//! |-------------|--------------------------------------------|----------|
//! | 0           | anchor-resolved (NULL `tasks.model`) escape valve | FEAT-004 |
//! | 1           | wave: ALL candidates quota-deferred, stale untouched | FEAT-008 |
//! | 2           | codex route + difficulty=high: effort capped, `-c` precedes `exec` | FEAT-006 |
//! | 3           | legacy keys: hard-error at loop/batch, warn on non-loop | FEAT-002 |
//!
//! Each not-yet-implemented behavior is `#[ignore]`d with the owning FEAT named;
//! that FEAT un-ignores the skeleton and makes it pass. The non-ignored tests
//! pin the CONTRACT-001 primitives the FEATs consume, so a regression in the
//! foundation surfaces immediately.
//!
//! Note on stderr assertions: the escape valve emits exactly one stderr line on
//! a clear. Capturing fd-2 from inside a libtest binary requires OS-level
//! redirection unavailable here, so "fires exactly once" is verified through
//! observable map state (the existing `tests/override_invalidation.rs` suite
//! takes the same approach).

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::commands::models::{SetDefaultOpts, handle_set_default};
use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::{
    CODEX_EFFORT_FOR_DIFFICULTY, CapabilityTier, FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, Provider,
    SONNET_MODEL, anchored_tier, provider_for_model, resolve_models_config,
};
use task_mgr::loop_engine::project_config::{
    ModelsConfig, RoutingConfig, detect_legacy_model_keys, preflight_validate_and_probe,
    read_project_config, validate_models_config,
};
use task_mgr::loop_engine::reactions::pre_spawn::invalidate_stale_overrides;
use task_mgr::loop_engine::runner::RunnerKind;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn setup_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let mut conn = open_connection(dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (dir, conn)
}

fn insert_task(conn: &Connection, id: &str, model: Option<&str>) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, max_retries, consecutive_failures) \
         VALUES (?, ?, 'in_progress', ?, ?, ?)",
        rusqlite::params![id, format!("Task {id}"), model, 5, 0],
    )
    .unwrap();
}

fn set_task_model(conn: &Connection, id: &str, model: Option<&str>) {
    conn.execute(
        "UPDATE tasks SET model = ? WHERE id = ?",
        rusqlite::params![model, id],
    )
    .unwrap();
}

/// Seed every per-task auto-recovery channel, mirroring what the overflow
/// ladder and the RuntimeError fallback hook write. `task_model_snapshot` is the
/// value the first-overflow snapshot recorded into `overflow_original_task_model`
/// — `None` is the NEW anchor-resolved semantics (task carried NULL `tasks.model`).
fn seed_all_overrides(ctx: &mut IterationContext, id: &str, task_model_snapshot: Option<&str>) {
    ctx.effort_overrides.insert(id.to_string(), "high");
    ctx.model_overrides
        .insert(id.to_string(), OPUS_MODEL.to_string());
    ctx.overflow_recovered.insert(id.to_string());
    ctx.overflow_original_model
        .insert(id.to_string(), OPUS_MODEL.to_string());
    ctx.runner_overrides
        .insert(id.to_string(), RunnerKind::Claude);
    ctx.overflow_original_task_model
        .insert(id.to_string(), task_model_snapshot.map(str::to_string));
}

fn assert_all_overrides_cleared(ctx: &IterationContext, id: &str) {
    assert!(
        !ctx.effort_overrides.contains_key(id),
        "effort_overrides[{id}]"
    );
    assert!(
        !ctx.model_overrides.contains_key(id),
        "model_overrides[{id}]"
    );
    assert!(
        !ctx.overflow_recovered.contains(id),
        "overflow_recovered[{id}]"
    );
    assert!(
        !ctx.overflow_original_model.contains_key(id),
        "overflow_original_model[{id}]"
    );
    assert!(
        !ctx.runner_overrides.contains_key(id),
        "runner_overrides[{id}]"
    );
    assert!(
        !ctx.overflow_original_task_model.contains_key(id),
        "overflow_original_task_model[{id}]"
    );
}

fn assert_all_overrides_present(ctx: &IterationContext, id: &str) {
    assert!(
        ctx.effort_overrides.contains_key(id),
        "effort_overrides[{id}]"
    );
    assert!(
        ctx.model_overrides.contains_key(id),
        "model_overrides[{id}]"
    );
    assert!(
        ctx.overflow_recovered.contains(id),
        "overflow_recovered[{id}]"
    );
    assert!(
        ctx.overflow_original_model.contains_key(id),
        "overflow_original_model[{id}]"
    );
    assert!(
        ctx.runner_overrides.contains_key(id),
        "runner_overrides[{id}]"
    );
    assert!(
        ctx.overflow_original_task_model.contains_key(id),
        "overflow_original_task_model[{id}]"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// NON-IGNORED — pin the CONTRACT-001 primitives the four FEATs consume.
// ════════════════════════════════════════════════════════════════════════════

/// `anchored_tier` is the single SSoT for anchor + difficulty-offset selection
/// (FEAT-004/007 consume it). `low → −1`, `medium → 0`, `high → +1`, clamped at
/// the ladder ends so the window never wraps.
#[test]
fn anchored_tier_window_and_clamps() {
    use CapabilityTier::*;

    // anchor=standard window.
    assert_eq!(anchored_tier(Standard, Some("low")), CostEfficient);
    assert_eq!(anchored_tier(Standard, Some("medium")), Standard);
    assert_eq!(anchored_tier(Standard, Some("high")), Frontier);
    // absent / unknown difficulty contributes offset 0.
    assert_eq!(anchored_tier(Standard, None), Standard);
    assert_eq!(anchored_tier(Standard, Some("bogus")), Standard);

    // Clamp at the ends — never wrap.
    assert_eq!(anchored_tier(Cheapest, Some("low")), Cheapest);
    assert_eq!(anchored_tier(Frontier, Some("high")), Frontier);
    // A frontier anchor with low difficulty still steps down one rung only.
    assert_eq!(anchored_tier(Frontier, Some("low")), Standard);
}

/// `CapabilityTier::parse` is config exact-match; the legacy Claude-family alias
/// keys (`opus`/`sonnet`/`haiku`) are a CONFIG ERROR naming the accepted set,
/// never a silent fall-through (prohibited-outcome: no legacy alias tier keys).
#[test]
fn capability_tier_parse_rejects_legacy_aliases() {
    for alias in ["opus", "sonnet", "haiku", "cost_efficient", "fronteir", ""] {
        let err = CapabilityTier::parse(alias).unwrap_err();
        assert!(
            err.contains("cheapest")
                && err.contains("cost-efficient")
                && err.contains("standard")
                && err.contains("frontier"),
            "parse error for {alias:?} must name the accepted set: {err}"
        );
    }
    // The canonical kebab-case keys parse.
    assert_eq!(
        CapabilityTier::parse("cheapest").unwrap(),
        CapabilityTier::Cheapest
    );
    assert_eq!(
        CapabilityTier::parse(" Cost-Efficient ").unwrap(),
        CapabilityTier::CostEfficient
    );
    assert_eq!(
        CapabilityTier::parse("frontier").unwrap(),
        CapabilityTier::Frontier
    );
}

/// `provider_for_model` classifies by token equality and NEVER returns
/// `Provider::Codex` — Codex is config-explicit only (prohibited-outcome: Codex
/// inferred from a model string).
#[test]
fn provider_for_model_never_returns_codex() {
    for m in [
        Some("gpt-5-codex"),
        Some("o3"),
        Some("codex-mini"),
        Some("openai-frontier"),
        Some(OPUS_MODEL),
        Some(FABLE_MODEL),
        None,
        Some(""),
    ] {
        assert_ne!(
            provider_for_model(m),
            Provider::Codex,
            "{m:?} must never classify as Codex"
        );
    }
    // Token-equality routing for the two inferable providers.
    assert_eq!(provider_for_model(Some("grok-build")), Provider::Grok);
    assert_eq!(provider_for_model(Some(OPUS_MODEL)), Provider::Claude);
    // Groq Inc. family contains the substring but not the token "grok".
    assert_eq!(provider_for_model(Some("groq-llama-3")), Provider::Claude);
}

/// `detect_legacy_model_keys` names each present legacy key in canonical order —
/// the detection primitive FEAT-002 feeds into both the hard-error and the
/// warn-once messages.
#[test]
fn detect_legacy_model_keys_names_each_present_key() {
    let legacy = serde_json::json!({
        "defaultModel": OPUS_MODEL,
        "reviewModel": SONNET_MODEL,
        "primaryRunner": {},
        "fallbackRunner": {},
        "models": {},
        "additionalAllowedTools": []
    });
    assert_eq!(
        detect_legacy_model_keys(&legacy),
        vec![
            "defaultModel",
            "reviewModel",
            "primaryRunner",
            "fallbackRunner"
        ]
    );
    // Clean post-migration config and non-objects → empty.
    assert!(detect_legacy_model_keys(&serde_json::json!({ "models": {} })).is_empty());
    assert!(detect_legacy_model_keys(&serde_json::json!([1, 2, 3])).is_empty());
}

/// `validate_models_config` rejects a codex `xhigh` effort entry by policy,
/// naming the offending key (prohibited-outcome: xhigh must never reach codex).
#[test]
fn validate_models_config_rejects_codex_xhigh_effort() {
    // Production-shaped config (FR-001 JSON schema), deserialized — never a
    // hand-built struct map.
    let models_json = serde_json::json!({
        "primaryProvider": "codex",
        "anchor": "standard",
        "providers": {
            "codex": {
                "enabled": true,
                "tiers": { "standard": null },
                "effort": { "high": "xhigh" }
            }
        }
    });
    let models: ModelsConfig = serde_json::from_value(models_json).unwrap();
    let errs = validate_models_config(&models, &RoutingConfig::default()).unwrap_err();
    assert!(
        errs.iter().any(|e| {
            e.contains("codex")
                && e.contains("xhigh")
                && e.contains("policy")
                && e.contains("effort")
        }),
        "expected a codex xhigh policy rejection naming the key: {errs:?}"
    );
}

/// The codex effort table is capped at `high`: difficulty=high resolves effort
/// `high`, never `xhigh`. Pins the FEAT-006 effort-cap half (the argv-ordering
/// half is the FEAT-006 skeleton below).
#[test]
fn codex_effort_table_caps_at_high() {
    // The raw table never emits xhigh.
    assert!(
        !CODEX_EFFORT_FOR_DIFFICULTY
            .iter()
            .any(|(_, e)| *e == "xhigh"),
        "CODEX_EFFORT_FOR_DIFFICULTY must never map to xhigh"
    );
    let high = CODEX_EFFORT_FOR_DIFFICULTY
        .iter()
        .find(|(d, _)| *d == "high")
        .map(|(_, e)| *e);
    assert_eq!(high, Some("high"));

    // The resolved view (production-shaped builtin default) agrees.
    let resolved =
        resolve_models_config(&ModelsConfig::builtin_default(), &RoutingConfig::default());
    assert_eq!(
        resolved.effort_for(Provider::Codex, Some("high")),
        Some("high")
    );
    assert_eq!(
        resolved.effort_for(Provider::Codex, Some("medium")),
        Some("medium")
    );
    assert_ne!(
        resolved.effort_for(Provider::Codex, Some("high")),
        Some("xhigh")
    );
}

/// EXISTING-function half of edgeCases[0]: an anchor-resolved task whose snapshot
/// recorded NULL still fires the six-channel clear when an operator stamps an
/// explicit model out-of-band — `Some(None) != Some(Some(model))`. This is the
/// behavior `invalidate_stale_overrides` already has; the FEAT-004 skeleton below
/// pins the part it does NOT yet have (absorbing the ladder's own write).
#[test]
fn null_snapshot_then_operator_stamp_fires_six_channel_clear() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "ANCHOR-NULL-001", None);

    let mut ctx = IterationContext::new(5);
    // Anchor-resolved: the first-overflow snapshot recorded NULL.
    seed_all_overrides(&mut ctx, "ANCHOR-NULL-001", None);

    // Operator stamps an explicit model out-of-band.
    set_task_model(&conn, "ANCHOR-NULL-001", Some(SONNET_MODEL));
    invalidate_stale_overrides(&mut ctx, &conn, "ANCHOR-NULL-001");

    assert_all_overrides_cleared(&ctx, "ANCHOR-NULL-001");
}

// ════════════════════════════════════════════════════════════════════════════
// IGNORED SKELETONS — one per edge case, each named for its owning FEAT.
// ════════════════════════════════════════════════════════════════════════════

/// edgeCases[0] (owner FEAT-004) — escape-valve NULL-original semantics.
///
/// An anchor-resolved task carries NULL `tasks.model`; first overflow snapshots
/// the ORIGINAL model as NULL. The escalation ladder then writes `tasks.model`
/// (e.g. Opus) — that is the ladder's OWN write, NOT an operator edit, so the
/// next `invalidate_stale_overrides` must be a no-op. Only a SUBSEQUENT operator
/// edit fires the six-channel clear — exactly once, with one stderr line.
///
/// Fails today: the current comparison (`Some(None) != Some(Some(opus))`) fires
/// on the ladder's own write at step 3. FEAT-004's NULL-original semantics make
/// the snapshot absorb that write so the clear fires exactly once at step 4.
#[ignore = "un-ignored by FEAT-004: NULL-original snapshot must absorb the ladder's own tasks.model write so only a later operator edit fires the six-channel clear"]
#[test]
fn edge_case_0_anchor_resolved_null_escape_valve_fires_exactly_once() {
    let (_dir, conn) = setup_db();
    // (1) anchor-resolved task: NULL tasks.model.
    insert_task(&conn, "ESCAPE-NULL-001", None);

    let mut ctx = IterationContext::new(5);
    // (2) first overflow snapshots the ORIGINAL model = NULL.
    seed_all_overrides(&mut ctx, "ESCAPE-NULL-001", None);

    // (3) escalation ladder writes tasks.model — NOT an operator edit.
    set_task_model(&conn, "ESCAPE-NULL-001", Some(OPUS_MODEL));
    invalidate_stale_overrides(&mut ctx, &conn, "ESCAPE-NULL-001");
    assert_all_overrides_present(&ctx, "ESCAPE-NULL-001");

    // (4) operator edits tasks.model out-of-band — the ONE legitimate fire.
    set_task_model(&conn, "ESCAPE-NULL-001", Some(SONNET_MODEL));
    invalidate_stale_overrides(&mut ctx, &conn, "ESCAPE-NULL-001");
    assert_all_overrides_cleared(&ctx, "ESCAPE-NULL-001");
}

/// edgeCases[1] (owner FEAT-008) — deferral-first, parity in BOTH paths.
///
/// A wave/iteration whose only candidates are quota-deferred must wait for the
/// reset (existing `wait_for_usage_reset` machinery) and NEVER increment the
/// stale-abort counter. Historically an empty selection fed the stale tracker
/// (learning 3927), false-aborting with "no eligible tasks after 3 consecutive
/// stale iterations". The invariant must hold in BOTH the sequential
/// (`orchestrator::run_loop` NoEligibleTasks branch) and the wave
/// (`wave_orchestration::handle_no_eligible_tasks`) paths — parameter threading
/// between them diverges silently (learning 4913).
///
/// FEAT-008 wiring: build a quota-deferred selection (all candidates blacked out
/// via `provider_blackouts`) and drive each path's no-eligible handler, then
/// assert `stale_tracker.count()` is still 0. `provider_blackouts` must never
/// read or write `runner_overrides` (see `blackout_reroute_*` discriminator).
#[ignore = "un-ignored by FEAT-008: deferral-first branch (wave handle_no_eligible_tasks + sequential parity) must wait for quota reset without marking stale"]
#[test]
fn edge_case_1_all_candidates_quota_deferred_stale_counter_untouched_both_paths() {
    let ctx = IterationContext::new(5);
    // Baseline: a fresh tracker has counted nothing.
    assert_eq!(ctx.stale_tracker.count(), 0);

    // <FEAT-008: drive the SEQUENTIAL no-eligible handler under a full quota
    //  blackout here, then assert the counter is still 0.>
    assert_eq!(
        ctx.stale_tracker.count(),
        0,
        "sequential deferral-first path must not increment the stale counter"
    );

    // <FEAT-008: drive the WAVE handle_no_eligible_tasks under a full quota
    //  blackout here (same fixture), then assert the counter is still 0.>
    assert_eq!(
        ctx.stale_tracker.count(),
        0,
        "wave deferral-first path must not increment the stale counter (parity)"
    );
}

/// edgeCases[2] (owner FEAT-006) — codex effort cap + argv ordering.
///
/// A codex route at difficulty=high resolves effort `high` (never `xhigh` — see
/// `codex_effort_table_caps_at_high` / `validate_models_config_rejects_codex_xhigh_effort`),
/// and `build_codex_argv` must emit `-c model_reasoning_effort=high` BEFORE the
/// `exec` subcommand (the codex CLI requires `-c` overrides ahead of the
/// subcommand).
///
/// FEAT-006 wiring (build_codex_argv gains an effort parameter, not present yet):
/// ```ignore
/// let argv = build_codex_argv(&mode, cwd, model, Some("high"));
/// let c_idx = argv.iter().position(|a| a == "-c").unwrap();
/// let effort_idx = argv.iter().position(|a| a == "model_reasoning_effort=high").unwrap();
/// let exec_idx = argv.iter().position(|a| a == "exec").unwrap();
/// assert!(c_idx < exec_idx && effort_idx < exec_idx);
/// assert!(!argv.iter().any(|a| a.contains("xhigh")));
/// ```
#[ignore = "un-ignored by FEAT-006: build_codex_argv gains effort; -c model_reasoning_effort=<level> must precede the exec subcommand and difficulty=high caps at high"]
#[test]
fn edge_case_2_codex_effort_flag_precedes_exec_and_caps_at_high() {
    // Reachable proxy for the cap the argv builder will honor: difficulty=high
    // maps to effort "high", never "xhigh".
    let high = CODEX_EFFORT_FOR_DIFFICULTY
        .iter()
        .find(|(d, _)| *d == "high")
        .map(|(_, e)| *e);
    assert_eq!(high, Some("high"));
    assert!(
        !CODEX_EFFORT_FOR_DIFFICULTY
            .iter()
            .any(|(_, e)| *e == "xhigh")
    );
}

/// edgeCases[3] (owner FEAT-002) — legacy keys: hard-error vs warn-and-proceed.
///
/// Legacy model-config keys (`defaultModel`/`reviewModel`/`primaryRunner`/
/// `fallbackRunner`) get DIFFERENT treatment by entry point:
///   * loop/batch entry + `models` mutating verbs → HARD ERROR naming each
///     present key, printing the new schema skeleton, pointing at
///     `models init --force-replace-legacy`.
///   * non-loop reads (`recall`, `models show`, `read_project_config`) → warn
///     ONCE on stderr and proceed with a usable config.
///
/// FEAT-002 wiring: assert the loop/batch preflight returns Err naming each
/// legacy key, and that `models` mutating verbs hard-error the same way.
#[test]
fn edge_case_3_legacy_keys_hard_error_at_loop_warn_on_nonloop() {
    let dir = TempDir::new().unwrap();
    let legacy = serde_json::json!({
        "defaultModel": OPUS_MODEL,
        "reviewModel": SONNET_MODEL,
        "primaryRunner": {},
        "fallbackRunner": {}
    });

    // Detection primitive (CONTRACT-001) names each key — FEAT-002 feeds this
    // into both the hard-error and the warn-once messages.
    assert_eq!(
        detect_legacy_model_keys(&legacy),
        vec![
            "defaultModel",
            "reviewModel",
            "primaryRunner",
            "fallbackRunner"
        ]
    );

    // Non-loop read path: must PROCEED (return a config), never panic/abort.
    std::fs::write(dir.path().join("config.json"), legacy.to_string()).unwrap();
    let _cfg = read_project_config(dir.path());

    // Loop/batch entry: preflight HARD-ERRORS naming every present legacy key
    // and pointing at the migration command.
    let err = preflight_validate_and_probe(dir.path(), &_cfg)
        .expect_err("loop/batch preflight must reject a legacy-key config");
    let msg = format!("{err}");
    for key in [
        "defaultModel",
        "reviewModel",
        "primaryRunner",
        "fallbackRunner",
    ] {
        assert!(msg.contains(key), "preflight error must name {key}: {msg}");
    }
    assert!(
        msg.contains("models init --force-replace-legacy"),
        "preflight error must point at the migration command: {msg}"
    );

    // `models` mutating verbs hard-error the same way (interim guard until
    // FEAT-009 replaces them).
    let verb_err = handle_set_default(
        dir.path(),
        SetDefaultOpts {
            model: Some(OPUS_MODEL.to_string()),
            project: true,
        },
    )
    .expect_err("models mutating verb must refuse a legacy-key config");
    let verb_msg = format!("{verb_err}");
    for key in [
        "defaultModel",
        "reviewModel",
        "primaryRunner",
        "fallbackRunner",
    ] {
        assert!(
            verb_msg.contains(key),
            "mutating-verb error must name {key}: {verb_msg}"
        );
    }
}

/// AC #6 (owner FEAT-008) — known-bad discriminator: the quota-blackout reroute
/// channel is EPHEMERAL and must never read or write `runner_overrides`, the
/// single permanent cross-provider promotion channel owned by `promote_once`
/// (learning 4921/4672). A blackout reroute that promoted via `runner_overrides`
/// would permanently pin the task to the spillover provider for the rest of the
/// run — exactly the known-bad this test catches.
#[ignore = "un-ignored by FEAT-008: provider_blackouts reroute must leave runner_overrides untouched (promote_once stays the single permanent-promotion guard)"]
#[test]
fn blackout_reroute_leaves_runner_overrides_untouched() {
    let ctx = IterationContext::new(5);
    assert!(ctx.runner_overrides.is_empty());

    // <FEAT-008: apply a provider_blackouts quota reroute for a deferred task
    //  here, then assert runner_overrides is STILL empty.>

    assert!(
        ctx.runner_overrides.is_empty(),
        "blackout reroute must NOT write the permanent-promotion channel; \
         an implementation that promotes via runner_overrides fails this test"
    );
}

// ── Compile marker ────────────────────────────────────────────────────────────

/// If the file stops building, this test disappears and the gap is visible in
/// the report. Also exercises both seed/assert helpers so they cannot rot.
#[test]
fn test_file_compiles_marker() {
    let mut ctx = IterationContext::new(1);
    seed_all_overrides(&mut ctx, "COMPILE-MARK", Some(HAIKU_MODEL));
    assert_all_overrides_present(&ctx, "COMPILE-MARK");
    assert_ne!(OPUS_MODEL, HAIKU_MODEL);
}
