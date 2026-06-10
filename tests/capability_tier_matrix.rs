//! Pure-layer edge-case suite pinning the CONTRACT-001 model-selection surface
//! (`CapabilityTier`, `ResolvedModelsConfig`, `anchored_tier`,
//! `provider_for_model`, the `models`/`routing` serde types).
//!
//! TDD intent: the foundation already implements these, so the suite passes
//! immediately — it exists to *pin* the contract so downstream FEATs cannot
//! regress it. Every assertion checks concrete content (exact model id / tier),
//! never `is_some()`-only.
//!
//! Model-ID discipline: this file lives outside `src/loop_engine/model.rs`, so
//! it must never contain a literal Claude model string (the
//! `no_hardcoded_models` regex `claude-(opus|sonnet|haiku|fable)-\d` would flag
//! it). Every Claude id comes from the exported constants, and the
//! substring-style probe strings are *built* from `FABLE_MODEL` at runtime.

use serde_json::{Value, json};

use task_mgr::loop_engine::model::{
    CapabilityTier, FABLE_MODEL, HAIKU_MODEL, ONE_M_SUFFIX, OPUS_MODEL, Provider, SONNET_MODEL,
    anchored_tier, provider_for_model, resolve_models_config,
};
use task_mgr::loop_engine::project_config::{
    ModelsConfig, RoutingConfig, merge_models_config, validate_models_config,
};

// ============================================================================
// Fixtures — production-shaped (FR-001 canonical JSON, real serde field names).
// NEVER hand-built typed maps: a snake_case-vs-kebab-case tier-key bug would
// silently pass a typed map but is caught the moment we go through serde.
// ============================================================================

/// The FR-001 canonical `models` block, verbatim shape, with model ids supplied
/// by the source-of-truth constants. Field names (`primaryProvider`,
/// `cost-efficient`, `cliBinary`, …) are exactly the serde wire form.
fn fr001_canonical_models_json() -> Value {
    json!({
        "primaryProvider": "claude",
        "anchor": "standard",
        "providers": {
            "claude": {
                "enabled": true,
                "tiers": {
                    "cheapest": HAIKU_MODEL,
                    "cost-efficient": SONNET_MODEL,
                    "standard": OPUS_MODEL,
                    "frontier": FABLE_MODEL
                },
                "effort": { "low": "medium", "medium": "high", "high": "high" },
                "fallback": null,
                "cliBinary": null
            },
            "grok": {
                "enabled": false,
                "tiers": { "standard": "grok-build" }
            },
            "codex": {
                "enabled": false,
                "tiers": { "standard": null },
                "effort": { "low": "low", "medium": "medium", "high": "high" }
            }
        }
    })
}

/// Resolve the built-in default `models` block (Claude full ladder, grok/codex
/// present-but-disabled, anchor=standard) against default routing.
fn resolved_default() -> task_mgr::loop_engine::model::ResolvedModelsConfig {
    let models = ModelsConfig::builtin_default();
    resolve_models_config(&models, &RoutingConfig::default())
}

/// Deserialize a production-shaped `models` value (no built-in merge — the
/// caller controls every rung, needed for sparse / gap ladders).
fn models_from_json(value: Value) -> ModelsConfig {
    serde_json::from_value(value).expect("production-shaped models JSON deserializes")
}

// ============================================================================
// AC#1 — happy path: default config + anchor window resolves the success-metric
// matrix (anchor=standard and anchor=cost-efficient).
// ============================================================================

#[test]
fn happy_path_anchor_standard_difficulty_window() {
    let r = resolved_default();
    let anchor = CapabilityTier::Standard;

    // low → cost-efficient → sonnet, medium → standard → opus, high → frontier → fable.
    assert_eq!(
        r.model_for(Provider::Claude, anchored_tier(anchor, Some("low"))),
        Some(SONNET_MODEL)
    );
    assert_eq!(
        r.model_for(Provider::Claude, anchored_tier(anchor, Some("medium"))),
        Some(OPUS_MODEL)
    );
    assert_eq!(
        r.model_for(Provider::Claude, anchored_tier(anchor, Some("high"))),
        Some(FABLE_MODEL)
    );
}

#[test]
fn happy_path_anchor_cost_efficient_shifts_one_rung_down() {
    let r = resolved_default();
    let anchor = CapabilityTier::CostEfficient;

    // The whole window shifts down a rung: low → cheapest → haiku,
    // medium → cost-efficient → sonnet, high → standard → opus.
    assert_eq!(
        r.model_for(Provider::Claude, anchored_tier(anchor, Some("low"))),
        Some(HAIKU_MODEL)
    );
    assert_eq!(
        r.model_for(Provider::Claude, anchored_tier(anchor, Some("medium"))),
        Some(SONNET_MODEL)
    );
    assert_eq!(
        r.model_for(Provider::Claude, anchored_tier(anchor, Some("high"))),
        Some(OPUS_MODEL)
    );
}

// ============================================================================
// AC#2 — all five `edgeCases` entries, 1:1.
// ============================================================================

/// Edge case 1: the frontier model carrying a trailing `[1m]` context suffix in
/// `tasks.model` — `tier_of` strips the `[1m]` before the exact config match,
/// so it still reverse-maps to Frontier. (The model id is built from
/// `FABLE_MODEL` so no literal lands in this file.)
#[test]
fn edge_case_1m_suffix_strips_before_exact_tier_match() {
    let r = resolved_default();
    // Built from constants, not a literal (the `[1m]` form would not match the
    // model-string regex, but the bare id inside it would).
    let one_m = format!("{FABLE_MODEL}{ONE_M_SUFFIX}");
    assert_eq!(
        r.tier_of(Provider::Claude, &one_m),
        Some(CapabilityTier::Frontier),
        "[1m] suffix must be stripped before the exact tier match"
    );
    // Case-insensitive suffix is also stripped.
    let one_m_upper = format!("{FABLE_MODEL}[1M]");
    assert_eq!(
        r.tier_of(Provider::Claude, &one_m_upper),
        Some(CapabilityTier::Frontier)
    );
}

/// Edge case 2: sparse grok ladder (only `standard` defined). An anchor window
/// that lands on `frontier` clamps by stepping DOWN to the defined rung; a query
/// at `cheapest` steps UP. Every tier resolves to the single defined rung.
#[test]
fn edge_case_sparse_grok_ladder_clamps_to_single_rung() {
    let r = resolved_default();
    let standard = r.model_for(Provider::Grok, CapabilityTier::Standard);
    assert!(
        standard.is_some(),
        "grok default exposes a single `standard` rung"
    );

    // Frontier (above) steps down; cheapest (below) steps up; both land on the
    // one defined rung — identical to the standard lookup.
    assert_eq!(
        r.model_for(Provider::Grok, CapabilityTier::Frontier),
        standard
    );
    assert_eq!(
        r.model_for(Provider::Grok, CapabilityTier::Cheapest),
        standard
    );
    assert_eq!(
        r.model_for(Provider::Grok, CapabilityTier::CostEfficient),
        standard
    );
}

/// Edge case 3: the difficulty offset saturates at the ladder ends and NEVER
/// wraps. The genuine off-the-end cases are `frontier + high` (+1 past the top,
/// pinned at `frontier`) and `cheapest + low` (-1 past the bottom, pinned at
/// `cheapest`) — matching `anchored_tier`'s own contract doc.
///
/// NOTE: the PRD `edgeCases` text reads "anchor=cheapest + high … clamped to
/// cheapest / anchor=frontier + low … clamped to frontier", which swaps the
/// difficulties — `cheapest + high` is a normal in-range +1 move to
/// `cost-efficient`, and `frontier + low` a normal -1 move to `standard`. The
/// CONTRACT-001 implementation is authoritative; this test pins the real
/// saturation behavior (the off-the-end cases) plus the in-range moves that
/// prove the clamp is not a no-op.
#[test]
fn edge_case_offset_clamps_at_ladder_ends_without_wrapping() {
    // Off-the-end: +1 past the top / -1 past the bottom stay pinned (saturate).
    assert_eq!(
        anchored_tier(CapabilityTier::Frontier, Some("high")),
        CapabilityTier::Frontier,
        "frontier + high (+1 past top) must saturate at frontier, never wrap to cheapest"
    );
    assert_eq!(
        anchored_tier(CapabilityTier::Cheapest, Some("low")),
        CapabilityTier::Cheapest,
        "cheapest + low (-1 past bottom) must saturate at cheapest, never wrap to frontier"
    );

    // In-range moves: prove the offset actually steps (not a clamped no-op).
    assert_eq!(
        anchored_tier(CapabilityTier::Cheapest, Some("high")),
        CapabilityTier::CostEfficient,
        "cheapest + high is a normal +1 move within the ladder"
    );
    assert_eq!(
        anchored_tier(CapabilityTier::Frontier, Some("low")),
        CapabilityTier::Standard,
        "frontier + low is a normal -1 move within the ladder"
    );
}

/// Edge case 4: single-tier ladder (codex default) always wins; a gap ladder
/// (only `cheapest` + `frontier` defined) clamps down-first across the gap,
/// then up. Exhaustive 4×tier matrix for the gap ladder.
#[test]
fn edge_case_single_and_gap_ladder_exhaustive_matrix() {
    let r = resolved_default();

    // --- single-tier codex ladder: the one rung is `null` (route with no model
    // flag), so every tier resolves to None but never panics. ---
    for tier in CapabilityTier::ALL {
        assert_eq!(
            r.model_for(Provider::Codex, tier),
            None,
            "codex single rung is null → no model flag at every tier"
        );
    }

    // --- gap ladder: only cheapest + frontier defined. ---
    let gap = models_from_json(json!({
        "primaryProvider": "claude",
        "anchor": "standard",
        "providers": {
            "claude": {
                "enabled": true,
                "tiers": { "cheapest": HAIKU_MODEL, "frontier": FABLE_MODEL }
            }
        }
    }));
    let rg = resolve_models_config(&gap, &RoutingConfig::default());

    // cheapest(0): defined         → haiku
    // cost-efficient(1): down to 0 → haiku  (down-first across the gap)
    // standard(2): down(1) undefined, up(3) defined → fable
    // frontier(3): defined         → fable
    assert_eq!(
        rg.model_for(Provider::Claude, CapabilityTier::Cheapest),
        Some(HAIKU_MODEL)
    );
    assert_eq!(
        rg.model_for(Provider::Claude, CapabilityTier::CostEfficient),
        Some(HAIKU_MODEL),
        "cost-efficient clamps DOWN across the gap to cheapest"
    );
    assert_eq!(
        rg.model_for(Provider::Claude, CapabilityTier::Standard),
        Some(FABLE_MODEL),
        "standard finds no rung below it within distance 1, then steps UP to frontier"
    );
    assert_eq!(
        rg.model_for(Provider::Claude, CapabilityTier::Frontier),
        Some(FABLE_MODEL)
    );
}

/// Edge case 5: `groq-llama-*` is Groq Inc., NOT xAI Grok. Token-equality
/// classification keeps it on Claude; substring matching would mis-route it.
#[test]
fn edge_case_groq_token_equality_stays_claude() {
    assert_eq!(provider_for_model(Some("groq-llama-70b")), Provider::Claude);
    assert_eq!(
        provider_for_model(Some("groq-llama-3.1-8b-instant")),
        Provider::Claude
    );
    // The genuine xAI token still classifies as Grok.
    assert_eq!(provider_for_model(Some("grok-build")), Provider::Grok);
    assert_eq!(provider_for_model(Some("grok-4-fast")), Provider::Grok);
}

// ============================================================================
// AC#3 — known-bad discriminator: substring-style ids must NOT reverse-map.
// ============================================================================

#[test]
fn known_bad_substring_model_does_not_reverse_map() {
    let r = resolved_default();

    // Built from the constant so no banned literal lands in source. A naive
    // `contains()` implementation would map this to Frontier; exact config
    // match (post-[1m]-strip) must reject it.
    let decoy = format!("my-{FABLE_MODEL}-custom");
    assert_eq!(
        r.tier_of(Provider::Claude, &decoy),
        None,
        "a superstring of a tier model must NOT reverse-map to that tier"
    );

    // Sanity: the EXACT id does map, proving the None above is discrimination,
    // not a blanket failure.
    assert_eq!(
        r.tier_of(Provider::Claude, FABLE_MODEL),
        Some(CapabilityTier::Frontier)
    );

    // A leading-substring decoy is likewise rejected.
    let decoy_prefix = format!("{FABLE_MODEL}-experimental");
    assert_eq!(r.tier_of(Provider::Claude, &decoy_prefix), None);
}

// ============================================================================
// AC#4 — invariants.
// ============================================================================

/// Invariant: clamping never wraps past the ladder ends, for the full
/// anchor × difficulty matrix.
#[test]
fn invariant_clamp_never_wraps_full_matrix() {
    for anchor in CapabilityTier::ALL {
        for difficulty in [Some("low"), Some("medium"), Some("high"), None] {
            let got = anchored_tier(anchor, difficulty);
            // Result is always a real ladder rung (Ord lets us bound-check).
            assert!(got >= CapabilityTier::Cheapest && got <= CapabilityTier::Frontier);
            // Never more than one rung from the anchor (offset window is ±1).
            let ai = CapabilityTier::ALL
                .iter()
                .position(|t| *t == anchor)
                .unwrap() as isize;
            let gi = CapabilityTier::ALL.iter().position(|t| *t == got).unwrap() as isize;
            assert!(
                (gi - ai).abs() <= 1,
                "anchor {anchor:?} + {difficulty:?} moved more than one rung"
            );
        }
    }
}

/// Invariant: `tier_of(model_for(p, t)) == Some(t)` for every DEFINED rung
/// (Claude's full ladder, each tier a distinct concrete model).
#[test]
fn invariant_tier_of_model_for_round_trips_every_defined_rung() {
    let r = resolved_default();
    for tier in CapabilityTier::ALL {
        let model = r
            .model_for(Provider::Claude, tier)
            .expect("every Claude rung is defined with a concrete model");
        assert_eq!(
            r.tier_of(Provider::Claude, model),
            Some(tier),
            "round-trip failed at {tier:?} (model {model})"
        );
    }
}

/// Invariant: `provider_for_model` token-equality behavior is unchanged — the
/// PRD must not perturb the classifier.
#[test]
fn invariant_provider_for_model_token_equality_unchanged() {
    assert_eq!(provider_for_model(Some("grok-build")), Provider::Grok);
    assert_eq!(provider_for_model(Some("grok-code-fast-1")), Provider::Grok);
    assert_eq!(provider_for_model(Some("groq-llama-70b")), Provider::Claude);
    assert_eq!(provider_for_model(Some(OPUS_MODEL)), Provider::Claude);
    assert_eq!(provider_for_model(Some(FABLE_MODEL)), Provider::Claude);
    assert_eq!(provider_for_model(Some("unknown-model")), Provider::Claude);
    assert_eq!(provider_for_model(Some("")), Provider::Claude);
    assert_eq!(provider_for_model(None), Provider::Claude);
}

// ============================================================================
// AC#5 — structural: the FR-001 canonical JSON deserializes (and validates).
// ============================================================================

#[test]
fn fr001_canonical_json_deserializes_into_models_config() {
    let value = fr001_canonical_models_json();
    let parsed: Result<ModelsConfig, _> = serde_json::from_value(value);
    assert!(
        parsed.is_ok(),
        "FR-001 canonical JSON must deserialize into ModelsConfig: {:?}",
        parsed.err()
    );

    // Content check: the kebab-case tier keys round-trip to the right models
    // through the resolved layer (proves field names + key format are correct,
    // not merely that *some* struct parsed).
    let cfg = parsed.unwrap();
    let resolved = resolve_models_config(&cfg, &RoutingConfig::default());
    assert_eq!(
        resolved.model_for(Provider::Claude, CapabilityTier::CostEfficient),
        Some(SONNET_MODEL)
    );
    assert_eq!(
        resolved.model_for(Provider::Claude, CapabilityTier::Frontier),
        Some(FABLE_MODEL)
    );

    // The canonical shape is itself semantically valid.
    assert!(
        validate_models_config(&cfg, &RoutingConfig::default()).is_ok(),
        "the FR-001 canonical config must pass validation"
    );
}

#[test]
fn builtin_default_merge_is_canonical_and_sparse_override_inherits() {
    // None / explicit null → the pure built-in default.
    assert_eq!(
        merge_models_config(None).unwrap(),
        ModelsConfig::builtin_default()
    );
    assert_eq!(
        merge_models_config(Some(&Value::Null)).unwrap(),
        ModelsConfig::builtin_default()
    );

    // A sparse `{enabled:true}` override keeps grok's default ladder (field-wise
    // merge never visits the untouched nested `tiers`).
    let merged = merge_models_config(Some(
        &json!({ "providers": { "grok": { "enabled": true } } }),
    ))
    .unwrap();
    let resolved = resolve_models_config(&merged, &RoutingConfig::default());
    let grok_standard = resolved.model_for(Provider::Grok, CapabilityTier::Standard);
    assert_eq!(
        grok_standard,
        resolved_default().model_for(Provider::Grok, CapabilityTier::Standard),
        "a sparse enabled-only override must inherit grok's default tier ladder"
    );
    assert!(grok_standard.is_some());
}

// ============================================================================
// failureModes — validation/lookup error behavior (cheap to pin here).
// ============================================================================

/// failureMode 1: an ambiguous reverse lookup (one model id mapped by two tiers
/// in a provider) is a CONFIG ERROR naming the offending model id.
#[test]
fn failure_mode_ambiguous_reverse_lookup_is_config_error() {
    let cfg = models_from_json(json!({
        "primaryProvider": "claude",
        "anchor": "standard",
        "providers": {
            "claude": {
                "enabled": true,
                "tiers": { "standard": OPUS_MODEL, "frontier": OPUS_MODEL }
            }
        }
    }));
    let errs = validate_models_config(&cfg, &RoutingConfig::default())
        .expect_err("duplicate model across two tiers must be rejected");
    assert!(
        errs.iter()
            .any(|e| e.contains(OPUS_MODEL) && e.contains("ambiguous")),
        "error must name the offending model id and flag ambiguity: {errs:?}"
    );
}

/// failureMode 2: a lookup on a provider with an empty/undefined ladder (and on
/// a provider absent from the config) returns None — no panic.
#[test]
fn failure_mode_empty_or_absent_ladder_returns_none() {
    // Provider present but with an empty tier map.
    let cfg = models_from_json(json!({
        "primaryProvider": "claude",
        "anchor": "standard",
        "providers": { "claude": { "enabled": true, "tiers": {} } }
    }));
    let r = resolve_models_config(&cfg, &RoutingConfig::default());
    for tier in CapabilityTier::ALL {
        assert_eq!(r.model_for(Provider::Claude, tier), None);
    }
    // Provider entirely absent from the config map.
    assert_eq!(r.model_for(Provider::Grok, CapabilityTier::Standard), None);
    assert_eq!(r.tier_of(Provider::Grok, "anything"), None);
}
