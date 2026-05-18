//! TDD scaffolding for US-002 / FR-003 — provider routing + escalation guards.
//!
//! Pins the token-equality algorithm for [`provider_for_model`] and the
//! Claude-only contract on the three escalation helpers
//! ([`escalate_model`], [`escalate_below_opus`], [`to_1m_model`]). Together
//! these are the **provider guard rail**: a Grok-tier model must never get
//! escalated to a Claude tier (which would silently route a Grok task back
//! through `ClaudeRunner` after Opus[1M] overflow), and a `groq-llama-*`
//! model from Groq Inc. (NOT xAI) must never be mis-routed to `GrokRunner`.
//!
//! ## What's locked in here
//!
//! 1. Token-equality, not substring matching. `lower.split('-').any(|t| t == "grok")`
//!    correctly classifies:
//!    - `grok-4`, `grok-4-fast`, `grok-code-fast-1`, `GROK-4`, `Grok-4-Fast`
//!      → `Provider::Grok`
//!    - `groq-llama-70b`, `groq-llama-3` (Groq Inc., NOT xAI) → `Provider::Claude`
//!    - `OPUS_MODEL`, `SONNET_MODEL`, `HAIKU_MODEL` (Claude model constants),
//!      `None`, `""`, `unknown-model`, `grokomatic-1`
//!      (single token `grokomatic` != `grok`) → `Provider::Claude`
//!
//! 2. The three Claude-only escalation helpers are no-ops on Grok inputs.
//!    FEAT-002 will add an explicit early-return `if provider_for_model(m) !=
//!    Provider::Claude { return None }` guard at the top of each function;
//!    today the same contract holds implicitly via [`model_tier`] returning
//!    `ModelTier::Default` for `grok-*` strings, so these assertions pass on
//!    main and on the post-FEAT-002 branch alike. The test serves as a
//!    contract lock: even if the model_tier implementation changes later,
//!    Grok inputs must continue to return `None` from these three functions.
//!
//! 3. Claude-side escalation is unchanged. `escalate_model(SONNET_MODEL) ==
//!    Some(OPUS_MODEL)` confirms the guard doesn't over-fire.
//!
//! ## File compiles
//!
//! Tests reference `task_mgr::loop_engine::model::{Provider, provider_for_model}`,
//! which TEST-INIT-002 introduces alongside the test file (the type and the
//! function are the minimal surface needed for the tests to compile). FEAT-002
//! will add the early-return guards inside the three escalation helpers and
//! a Provider-aware variant of the dispatch helper.

use task_mgr::loop_engine::model::{
    HAIKU_MODEL, OPUS_MODEL, OPUS_MODEL_1M, Provider, SONNET_MODEL, escalate_below_opus,
    escalate_model, provider_for_model, to_1m_model,
};

// ── provider_for_model: positive (Grok) cases ─────────────────────────────────

/// AC 1: every documented xAI Grok model id and case variant resolves to Grok.
#[test]
fn provider_for_model_positive_grok_ids() {
    let positives = [
        "grok-4",
        "grok-4-fast",
        "grok-code-fast-1",
        // Case-insensitive normalization (also covered by `mixed_case` test).
        "GROK-4",
        "Grok-4-Fast",
    ];
    for m in positives {
        assert_eq!(
            provider_for_model(Some(m)),
            Provider::Grok,
            "{m:?} should classify as Provider::Grok",
        );
    }
}

/// AC 4: lowercase normalization handles arbitrary case mixes.
#[test]
fn provider_for_model_mixed_case_normalizes_to_grok() {
    for m in ["GROK-4-FAST", "Grok-Code-Fast-1", "gRoK-4", "GroK-4-fast"] {
        assert_eq!(
            provider_for_model(Some(m)),
            Provider::Grok,
            "{m:?} should normalize to Provider::Grok via lowercase",
        );
    }
}

// ── provider_for_model: defensive (Groq Inc. ≠ xAI) cases ─────────────────────

/// AC 2: Groq Inc. models (NOT xAI Grok) must route to Claude.
///
/// Known-bad discriminator: a `.contains("grok")` implementation would
/// falsely match `groq-llama-3` and `groq-llama-70b` because `"grok"` is
/// not a contiguous substring of `"groq"`. Token-equality on `-` splits
/// rejects them cleanly — `groq != grok`.
#[test]
fn provider_for_model_defensive_groq_inc_routes_to_claude() {
    for m in [
        "groq-llama-70b",
        "groq-llama-3",
        "GROQ-llama-3",
        "groq-mixtral-8x7b",
    ] {
        assert_eq!(
            provider_for_model(Some(m)),
            Provider::Claude,
            "{m:?} (Groq Inc., NOT xAI) must NOT classify as Provider::Grok",
        );
    }
}

/// PRD §6 edge case: single token containing `grok` as a prefix (`grokomatic-1`)
/// is a non-xAI product. Token-equality means `grokomatic != grok` and the
/// classification falls through to Claude. A `.starts_with("grok")` or
/// `.contains("grok")` impl would mis-route here.
#[test]
fn provider_for_model_grok_prefix_token_is_not_xai() {
    for m in ["grokomatic-1", "grokster-7", "grokking-2", "grokster"] {
        assert_eq!(
            provider_for_model(Some(m)),
            Provider::Claude,
            "{m:?} (single non-matching token) must classify as Provider::Claude",
        );
    }
}

// ── provider_for_model: default (Claude) cases ────────────────────────────────

/// AC 3: Claude model constants, None, empty, and unknown strings all
/// fall through to Provider::Claude (the documented default).
#[test]
fn provider_for_model_default_cases_route_to_claude() {
    let claude_defaults: &[Option<&str>] = &[
        Some(OPUS_MODEL),
        Some(SONNET_MODEL),
        Some(HAIKU_MODEL),
        Some(OPUS_MODEL_1M),
        Some("unknown-model"),
        Some(""),
        Some("   "),
        Some("gpt-4"),
        Some("gemini-pro"),
        Some("llama-3-70b"),
        None,
    ];
    for m in claude_defaults {
        assert_eq!(
            provider_for_model(*m),
            Provider::Claude,
            "{m:?} should default to Provider::Claude",
        );
    }
}

/// Totality: provider_for_model never panics regardless of input.
/// The function must produce exactly one variant for every &str / None input;
/// arbitrary garbage must not crash, return placeholder errors, etc.
#[test]
fn provider_for_model_is_total_no_panic() {
    let nasties: &[Option<&str>] = &[
        None,
        Some(""),
        Some("\0"),
        Some("\t\n\r"),
        Some("---"),
        Some("-grok-"),
        Some("grok-"),
        Some("-grok"),
        Some("a--grok--b"),
        Some(&"a".repeat(10_000)),
    ];
    for m in nasties {
        let _ = provider_for_model(*m); // must not panic; result content not asserted here
    }
}

/// Hyphen-edge: leading/trailing hyphens and double-hyphens still yield a
/// `grok` token via split('-'). All of these are xAI Grok.
#[test]
fn provider_for_model_grok_with_hyphen_artifacts_is_grok() {
    for m in ["-grok", "grok-", "-grok-", "a--grok--b"] {
        assert_eq!(
            provider_for_model(Some(m)),
            Provider::Grok,
            "{m:?} contains a token equal to 'grok' — must classify as Provider::Grok",
        );
    }
}

// ── escalation guards: Grok inputs are a no-op ────────────────────────────────

/// AC 5: escalate_model on a Grok model returns None (provider guard).
///
/// If a future refactor of [`model_tier`] starts recognizing grok-* as
/// non-Default tier (or someone adds an explicit Grok escalation rung),
/// FEAT-002's explicit guard ensures the contract still holds. Today the
/// behavior is implicit via `ModelTier::Default` → `None`.
#[test]
fn escalate_model_on_grok_returns_none() {
    for m in [
        "grok-4-fast",
        "grok-4",
        "grok-code-fast-1",
        "GROK-4",
        "Grok-4-Fast",
    ] {
        assert_eq!(
            escalate_model(Some(m)),
            None,
            "escalate_model({m:?}) must return None (Grok cannot escalate via Claude ladder)",
        );
    }
}

/// AC 6: escalate_below_opus on a Grok model returns None.
#[test]
fn escalate_below_opus_on_grok_returns_none() {
    for m in ["grok-4", "grok-4-fast", "grok-code-fast-1"] {
        assert_eq!(
            escalate_below_opus(Some(m)),
            None,
            "escalate_below_opus({m:?}) must return None — Grok is not on the Claude tier ladder",
        );
    }
}

/// AC 7: to_1m_model on a Grok model returns None.
///
/// The 1M-context variant is an Opus-only Claude tier; Grok has no
/// equivalent and the helper must not pretend otherwise.
#[test]
fn to_1m_model_on_grok_returns_none() {
    for m in ["grok-4-fast", "grok-4", "grok-code-fast-1"] {
        assert_eq!(
            to_1m_model(Some(m)),
            None,
            "to_1m_model({m:?}) must return None — Grok has no 1M variant on the Claude ladder",
        );
    }
}

/// AC 8: claude-side escalation continues to work — confirms the guard
/// doesn't over-fire and break the existing Sonnet → Opus rung. This is the
/// "known-bad discriminator" for an over-zealous guard implementation: if a
/// future FEAT-002 patch accidentally writes `if provider_for_model(model) ==
/// Provider::Claude { return None; }` (inverted), this assertion catches it.
#[test]
fn escalate_model_claude_side_unaffected_by_guard() {
    assert_eq!(
        escalate_model(Some(SONNET_MODEL)),
        Some(OPUS_MODEL.to_string()),
        "Sonnet must still escalate to Opus — the provider guard must not over-fire",
    );
    assert_eq!(
        escalate_model(Some(HAIKU_MODEL)),
        Some(SONNET_MODEL.to_string()),
        "Haiku must still escalate to Sonnet — the provider guard must not over-fire",
    );
    assert_eq!(
        escalate_below_opus(Some(SONNET_MODEL)),
        Some(OPUS_MODEL),
        "Sonnet must still escalate-below-opus to Opus — guard must not over-fire",
    );
    assert_eq!(
        to_1m_model(Some(OPUS_MODEL)),
        Some(OPUS_MODEL_1M),
        "Opus must still produce its 1M variant — guard must not over-fire",
    );
}

/// AC 12: compile marker — runs every `cargo test --test provider_routing`
/// invocation so a build break surfaces as a missing-test signal rather than
/// silently being skipped along with other `#[ignore]`'d tests.
#[test]
fn provider_routing_test_file_compiles() {
    // If the build compiles to the point of running this test, the AC is met.
    assert_eq!(provider_for_model(None), Provider::Claude);
}
