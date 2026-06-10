//! Model selection and resolution logic for the loop engine.
//!
//! Pure functions for determining which Claude model to use for task execution.
//! No I/O dependencies — all inputs are passed as parameters.
//!
//! # Source of Truth
//!
//! This file is the canonical source of truth for Claude model IDs and the
//! difficulty→effort mapping. `.claude/commands/tasks.md` is regenerated from
//! this file by `cargo run --bin gen-docs`; CI enforces sync via
//! `cargo run --bin gen-docs -- --check`.
//!
//! When bumping a model ID or effort value, edit ONLY this file, then run
//! `cargo run --bin gen-docs` to refresh the slash-command doc. Tests import
//! these constants and pick up changes automatically.
//!
//! # Escalation vs. downgrade asymmetry
//!
//! Model escalation and effort downgrade are deliberately asymmetric.
//! [`escalate_tier`] moves the model up one DEFINED capability tier on the
//! provider's ladder (config exact-match, never substring) on repeat crash /
//! consecutive failure. [`downgrade_effort`] moves effort down one tier on
//! `PromptTooLong`. A task that starts at (Sonnet, `xhigh`) and keeps crashing
//! can therefore land at (Opus, `high`); this is intentional — `max` effort
//! was retired for overflowing context and the `xhigh → high` step is the
//! same safety valve for `xhigh`.
//!
//! When effort downgrade is exhausted (already at `high`), the overflow ladder
//! walks [`escalate_below_ceiling`] one tier at a time until the provider's
//! ladder is exhausted, then [`to_1m_model`] provides a final escape hatch:
//! append the 1M-context suffix to the (Claude-only) ceiling model for a larger
//! context window without changing effort.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::loop_engine::project_config::{
    ModelsConfig, RouteSpec, RoutingConfig, SpilloverConfig, TaskClassRoute,
};

/// Well-known model identifiers.
pub const OPUS_MODEL: &str = "claude-opus-4-8";
pub const SONNET_MODEL: &str = "claude-sonnet-4-6";
pub const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
/// Claude Fable 5 — the frontier-tier Claude model, above Opus. Added by the
/// model-selection redesign (FR-001); `claude-fable-5` contains no legacy tier
/// token, which is why substring tier classification is structurally dead and
/// `CapabilityTier`/`tier_of` use config exact-match instead.
pub const FABLE_MODEL: &str = "claude-fable-5";
/// Suffix marking a 1M-context model variant (`claude-opus-4-8[1m]`).
/// Stripped by `tier_of` before the exact config match and appended by the
/// Claude-only 1M escalation rung. Single source of truth for the literal.
pub const ONE_M_SUFFIX: &str = "[1m]";

/// LLM provider classification.
///
/// Computed from a model id by [`provider_for_model`] (token-equality on the
/// lowercased, hyphen-split model string). Used to pick the right
/// `RunnerKind` and to short-circuit Claude-only helpers like
/// [`escalate_tier`] when the active model belongs to a different provider.
///
/// New provider variants are added here when a new runner is plumbed
/// through `dispatch`; today the only two are Claude (default) and Grok
/// (FR-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Anthropic Claude models (default for unknown / unset inputs).
    Claude,
    /// xAI Grok models (`grok-build`, `grok-code-fast-1`, …).
    Grok,
    /// OpenAI Codex CLI models. In v1 this is selected only by explicit
    /// `primaryRunner.provider = "codex"`; model-name inference is forbidden.
    Codex,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Grok => "grok",
            Provider::Codex => "codex",
        }
    }
}

/// Parse a provider name from runner-routing config.
///
/// Strict, returns [`Result`] so that a typo or vendor look-alike
/// (`"openai"`, `"codex-cli"`, `"groq"`) produces a CONFIG ERROR at
/// validation time rather than a silent fall-through to Claude. Re-implementing
/// the mapping at another site risks accepting a substring like `"groq"` for
/// Grok or `"openai"` for Codex — keep all callers funneling through here.
///
/// # Contract
///
/// The accepted set is the exact set of values [`Provider::as_str`] returns,
/// so a `Provider` round-trips through `as_str → parse_config_provider`
/// unchanged. Whitespace and case are normalized before matching.
pub fn parse_config_provider(s: &str) -> Result<Provider, String> {
    let trimmed = s.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "claude" => Ok(Provider::Claude),
        "grok" => Ok(Provider::Grok),
        "codex" => Ok(Provider::Codex),
        _ => Err(format!(
            "unknown provider {trimmed:?} (expected one of: claude, grok, codex)"
        )),
    }
}

/// Classify a model id as a provider.
///
/// Algorithm: lowercase the input, split on `-`, return [`Provider::Grok`]
/// iff *some token is exactly* `"grok"`. Every other input — including
/// `None`, the empty string, `"unknown-model"`, the Claude model constants,
/// and Groq Inc. models like `"groq-llama-70b"` — falls through to
/// [`Provider::Claude`].
///
/// Token-equality (not substring matching) is load-bearing: `groq-llama-3`
/// from Groq Inc. is **not** an xAI product, and `.contains("grok")` would
/// mis-route it. Splitting on `-` and comparing each token to the literal
/// `"grok"` rejects Groq cleanly. Total function: every `Option<&str>`
/// input produces some `Provider`; never panics.
///
/// # Examples
///
/// ```ignore
/// use task_mgr::loop_engine::model::{Provider, provider_for_model};
/// assert_eq!(provider_for_model(Some("grok-build")), Provider::Grok);
/// assert_eq!(provider_for_model(Some("groq-llama-3")), Provider::Claude);
/// assert_eq!(provider_for_model(Some("claude-opus-4-7")), Provider::Claude);
/// assert_eq!(provider_for_model(None), Provider::Claude);
/// ```
pub fn provider_for_model(model: Option<&str>) -> Provider {
    let lower = model.unwrap_or("").to_ascii_lowercase();
    if lower.split('-').any(|t| t == "grok") {
        Provider::Grok
    } else {
        Provider::Claude
    }
}

/// Decide the `provider_hint` that survives to the spawn boundary.
///
/// Shared by the sequential (`iteration.rs`) and wave (`wave_scheduler.rs`)
/// dispatch sites so the post-plan drop rule cannot drift between them
/// (REFACTOR-009). The hint is the explicit provider intent carried from the
/// resolved [`ExecutionPlan`]; it is the ONLY carrier of Codex intent on the
/// provider-only spec `{provider:"codex",model:""}` (which normalises to
/// `resolved_model = None`).
///
/// Two cases the rule distinguishes:
///
/// - **Rewrite** — the model was replaced out from under the hint (crash
///   escalation, prior-overflow 1M, review routing). The new `--model` may
///   belong to a different provider, so a stale hint would mis-route → the
///   hint is **dropped**.
/// - **Widening** — `resolved_model` was `None` and `effective_model` merely
///   widened to `default_model`. This is not a provider change → the hint is
///   **preserved**, or a Codex task would dispatch to Claude.
///
/// The blanket `effective_model != resolved_model → None` form (the historical
/// sequential rule) was only correct because the sequential path never widens
/// `effective_model`; expressing the rule here makes adding `.or(default_model)`
/// to either path safe.
///
/// # Examples
///
/// ```ignore
/// use task_mgr::loop_engine::model::{Provider, final_provider_hint};
/// // Codex provider-only spec, model widened to the default — hint survives.
/// assert_eq!(
///     final_provider_hint(Some(Provider::Codex), Some("claude-opus-4-8"), None, Some("claude-opus-4-8")),
///     Some(Provider::Codex)
/// );
/// // Crash escalation rewrote the model — stale hint dropped.
/// assert_eq!(
///     final_provider_hint(Some(Provider::Grok), Some("claude-fable-5"), Some("grok-build"), Some("claude-opus-4-8")),
///     None
/// );
/// ```
pub fn final_provider_hint(
    plan_hint: Option<Provider>,
    effective_model: Option<&str>,
    resolved_model: Option<&str>,
    default_model: Option<&str>,
) -> Option<Provider> {
    let widened = resolved_model.is_none() && effective_model == default_model;
    if effective_model != resolved_model && !widened {
        None
    } else {
        plan_hint
    }
}

/// Strip the leading 8-lowercase-hex project prefix from a task ID.
///
/// Production task IDs carry an 8-lowercase-hex project prefix generated by
/// `prefix_id` in `src/commands/init/mod.rs` (e.g. `8d71d1f7-CODE-REVIEW-1`).
/// Only exactly 8 lowercase hex chars followed by `-` are stripped — stripping
/// on the first `-` generically would convert `REFACTOR-REVIEW-FINAL` to
/// `REVIEW-FINAL` and produce false positives in prefix checks.
///
/// Returns the body slice (without the prefix and its trailing `-`), or the
/// original `id` slice unchanged when no valid prefix is present.
fn strip_prd_prefix(id: &str) -> &str {
    if id.len() > 9
        && id.as_bytes()[8] == b'-'
        && id[..8]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        &id[9..]
    } else {
        id
    }
}

/// Return true when a task ID belongs to the review class that may be routed
/// to an alternate model via `ProjectConfig::review_model`.
///
/// Review-class prefixes: `CODE-REVIEW-`, `MILESTONE-FINAL`, `REVIEW-`.
/// Non-review (explicit exclusion): `REFACTOR-REVIEW-*` — checked first so
/// that `REVIEW-` cannot accidentally match it.
///
/// **Prefix stripping**: production task IDs carry an 8-lowercase-hex project
/// prefix (`<8 hex chars>-<TYPE>-<n>`) generated by `prefix_id` in
/// `src/commands/init/mod.rs`. This function strips a leading `[0-9a-f]{8}-`
/// group before matching — a naive `starts_with("CODE-REVIEW-")` would
/// silently no-op on every production ID.
///
/// Only exactly 8 lowercase hex chars followed by `-` are stripped. Stripping
/// on the first `-` generically would reduce `REFACTOR-REVIEW-FINAL` to
/// `REVIEW-FINAL` and produce a false positive.
///
/// This is the SSoT for the built-in review class (consumed by
/// [`classify_task`] → [`TaskClass::Review`]); it forces the frontier tier and
/// is NOT redefinable via config — hence the "frontier class" name.
pub fn is_frontier_class(id: &str) -> bool {
    let body = strip_prd_prefix(id);

    if body.starts_with("REFACTOR-REVIEW") {
        return false;
    }

    body.starts_with("CODE-REVIEW-")
        || body.starts_with("MILESTONE-FINAL")
        || body.starts_with("REVIEW-")
}

/// Mapping from task difficulty to Claude CLI `--effort` level.
///
/// Scaling: low → low, medium → medium, high → high.
/// Unset / unknown difficulty → no `--effort` flag (CLI default applies).
///
/// Intentionally capped at `high` (not `xhigh`): `xhigh` overshoots the
/// context budget on frontier models. On overflow, `downgrade_effort` steps
/// `high → medium`; `medium` is the floor.
pub const CLAUDE_EFFORT_FOR_DIFFICULTY: &[(&str, &str)] =
    &[("low", "low"), ("medium", "medium"), ("high", "high")];

/// Grok difficulty → `--effort` level.
///
/// Scaling: low → medium, medium → high, high → xhigh.
pub const GROK_EFFORT_FOR_DIFFICULTY: &[(&str, &str)] =
    &[("low", "medium"), ("medium", "high"), ("high", "xhigh")];

/// Static fallback used by prompt builders when no per-provider effort entry is
/// found. Identical to `GROK_EFFORT_FOR_DIFFICULTY`.
pub const EFFORT_FOR_DIFFICULTY: &[(&str, &str)] =
    &[("low", "medium"), ("medium", "high"), ("high", "xhigh")];

/// Codex difficulty → `-c model_reasoning_effort=` level.
///
/// Capped at `high` BY POLICY: the codex CLI itself accepts
/// `none|minimal|low|medium|high|xhigh` (spike-confirmed against codex-cli
/// 0.136.0, 2026-06-09), but the loop forbids `xhigh` so a runaway reasoning
/// budget can never be configured. `validate_models_config` rejects an `xhigh`
/// codex effort entry naming the policy. Unlike `EFFORT_FOR_DIFFICULTY` (Claude
/// /Grok), codex maps each difficulty to the same-named effort level.
/// The flag is emitted by build_codex_argv (BEFORE the exec subcommand) when
/// the selected runner supports Effort (now true for CodexRunner).
pub const CODEX_EFFORT_FOR_DIFFICULTY: &[(&str, &str)] =
    &[("low", "low"), ("medium", "medium"), ("high", "high")];

/// Default tier → model tables for the built-in providers (declarative source
/// for `cargo run --bin gen-docs` tier matrix + anchor docs). These mirror the
/// construction in `project_config::default_*_provider` but live here so docs
/// extraction and the no-hardcoded-models rule stay confined to model.rs.
/// Empty string value for codex "standard" signals "route with no model flag".
pub const CLAUDE_DEFAULT_TIER_MODELS: &[(&str, &str)] = &[
    ("cheapest", HAIKU_MODEL),
    ("cost-efficient", SONNET_MODEL),
    ("standard", OPUS_MODEL),
    ("frontier", FABLE_MODEL),
];
pub const GROK_DEFAULT_TIER_MODELS: &[(&str, &str)] = &[("standard", "grok-build")];
pub const CODEX_DEFAULT_TIER_MODELS: &[(&str, &str)] = &[("standard", "")];

/// Trim + lowercase a difficulty string for table lookup. Returns `None` when
/// the input is absent or whitespace-only so callers can short-circuit.
fn normalize_difficulty(difficulty: Option<&str>) -> Option<String> {
    let d = difficulty?.trim().to_ascii_lowercase();
    if d.is_empty() { None } else { Some(d) }
}

/// Map task difficulty to Claude CLI `--effort` level.
///
/// Looks the difficulty up in `EFFORT_FOR_DIFFICULTY` (case-insensitive,
/// whitespace-trimmed). Returns `None` for `None` or unknown difficulty —
/// the caller should then omit `--effort` and let Claude use its CLI default.
pub fn effort_for_difficulty(difficulty: Option<&str>) -> Option<&'static str> {
    let d = normalize_difficulty(difficulty)?;
    EFFORT_FOR_DIFFICULTY
        .iter()
        .find(|(k, _)| *k == d)
        .map(|(_, v)| *v)
}

/// Rank a difficulty by its position in `EFFORT_FOR_DIFFICULTY`.
///
/// Higher index = harder difficulty (ladder is ascending by construction).
/// `None`, empty, or unknown strings return `None` — they don't participate
/// in cluster-wide max comparisons.
pub fn difficulty_rank(difficulty: Option<&str>) -> Option<usize> {
    let d = normalize_difficulty(difficulty)?;
    EFFORT_FOR_DIFFICULTY.iter().position(|(k, _)| *k == d)
}

/// Normalize a route prefix for matching: trim whitespace + a trailing `-`
/// (so `"REVIEW"` and `"REVIEW-"` behave identically).
fn normalize_route_prefix(prefix: &str) -> String {
    prefix.trim().trim_end_matches('-').to_string()
}

/// Dash-boundary prefix match against a task ID body (after the 8-hex project
/// prefix is stripped): the prefix must match a contiguous run of dash-delimited
/// segments anywhere in the body (not anchored to the start), so `"REVIEW-"`
/// matches both `REVIEW-001` and `CODE-REVIEW-001` but not `REVIEWER-001`.
/// Kept local to `model` (rather than reusing
/// `commands::next::selection::id_body_matches_prefix`) to avoid a
/// `loop_engine` → `commands` layer dependency.
fn id_body_matches_prefix(body: &str, prefix: &str) -> bool {
    let prefix = normalize_route_prefix(prefix);
    if prefix.is_empty() {
        return false;
    }
    let body_segments: Vec<&str> = body.split('-').collect();
    let prefix_segments: Vec<&str> = prefix.split('-').collect();
    segments_contain(&body_segments, &prefix_segments)
}

fn segments_contain(haystack: &[&str], needle: &[&str]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn normalize(s: Option<&str>) -> Option<&str> {
    s.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() { None } else { Some(v) }
    })
}

/// Step one tier down on the effort ladder.
///
/// Currently only `xhigh → high` is defined — `high` is the floor for overflow
/// recovery. All other inputs (including `high`, `medium`, `low`, `None`,
/// unknown strings) return `None`, signalling "no downgrade available". New
/// tiers can be added without touching callers.
pub fn downgrade_effort(effort: Option<&str>) -> Option<&'static str> {
    match effort {
        Some("xhigh") => Some("high"),
        _ => None,
    }
}

/// Check whether a model string is already a 1M-context variant (contains `[1m]`).
pub fn is_1m_model(model: Option<&str>) -> bool {
    model.is_some_and(|m| m.to_lowercase().contains("[1m]"))
}

/// Return the 1M-context variant for a Claude model.
///
/// Suffix-append (NOT a constant lookup): appends [`ONE_M_SUFFIX`] to ANY
/// Claude model so the 1M rung covers every Claude tier without a per-model
/// constant — `claude-opus-4-8 → claude-opus-4-8[1m]`,
/// `claude-fable-5 → claude-fable-5[1m]`. Returns `None` when the model is
/// already a 1M variant, is not a Claude model (provider guard), or is `None`.
/// 1M context is a Claude-only capability; Grok/Codex have no equivalent.
pub fn to_1m_model(model: Option<&str>) -> Option<String> {
    let m = model.map(str::trim).filter(|s| !s.is_empty())?;
    if provider_for_model(Some(m)) != Provider::Claude {
        return None;
    }
    if is_1m_model(Some(m)) {
        return None;
    }
    Some(format!("{m}{ONE_M_SUFFIX}"))
}

/// The next DEFINED capability tier strictly above `from` on `provider`'s
/// sparse ladder in `resolved`, or `None` when `from` is already the highest
/// defined rung. Undefined rungs are skipped — escalation always lands on a
/// rung the provider actually maps to a model.
fn next_defined_tier_above(
    resolved: &ResolvedModelsConfig,
    provider: Provider,
    from: CapabilityTier,
) -> Option<CapabilityTier> {
    let p = resolved.providers.get(&provider)?;
    CapabilityTier::ALL
        .iter()
        .copied()
        .filter(|t| *t > from)
        .find(|t| p.tiers.contains_key(t))
}

/// Escalate `model` up exactly one DEFINED tier on `provider`'s ladder.
///
/// Tier membership is config exact-match (via [`ResolvedModelsConfig::tier_of`]),
/// never substring matching, and the ladder is whatever the provider's `tiers`
/// map defines. At the highest defined tier it **self-loops** (returns the model
/// for the current tier — no wrap to the bottom, no hop to another provider).
/// Returns `None` only when `model`'s tier cannot be resolved for `provider`
/// (off-ladder model, or the provider is absent / has no defined rung).
pub fn escalate_tier(
    resolved: &ResolvedModelsConfig,
    provider: Provider,
    model: Option<&str>,
) -> Option<String> {
    let current = resolved.tier_of(provider, model?)?;
    let target = next_defined_tier_above(resolved, provider, current).unwrap_or(current);
    resolved.model_for(provider, target).map(str::to_string)
}

/// Escalate `model` up one DEFINED tier on `provider`'s ladder, returning
/// `None` AT THE CEILING instead of self-looping.
///
/// This is the overflow ladder's model-escalation rung: the caller must be able
/// to distinguish "moved up a tier" (`Some`) from "ladder exhausted" (`None`)
/// to fall through to the 1M-context rung. For a single-rung provider (Grok,
/// Codex) the model already sits at the ceiling, so this returns `None` and the
/// overflow ladder advances to the cross-provider fallback rung — no model
/// escalation, no provider hop within the rung.
pub fn escalate_below_ceiling(
    resolved: &ResolvedModelsConfig,
    provider: Provider,
    model: Option<&str>,
) -> Option<String> {
    let current = resolved.tier_of(provider, model?)?;
    let target = next_defined_tier_above(resolved, provider, current)?;
    resolved.model_for(provider, target).map(str::to_string)
}

/// The built-in default resolved config (Claude full ladder enabled, grok/codex
/// present-but-disabled, `anchor=standard`), built once and cached.
///
/// Recovery paths that lack a threaded `ProjectConfig` (the consecutive-failure
/// escalation in `recovery.rs`, whose call sites predate the provider-first
/// config) resolve the Claude ladder through this so they stay config-driven
/// (no hardcoded Claude-constant ladder) without changing their signatures.
/// The overflow ladder, which DOES carry `project_config`, resolves the real
/// user config instead — never this builtin.
pub fn builtin_resolved_models() -> &'static ResolvedModelsConfig {
    use std::sync::OnceLock;
    static C: OnceLock<ResolvedModelsConfig> = OnceLock::new();
    C.get_or_init(|| {
        resolve_models_config(
            crate::loop_engine::project_config::default_models_config(),
            crate::loop_engine::project_config::default_routing_config(),
        )
    })
}

// ============================================================================
// Capability tiers + resolved models config (FR-001)
//
// The keystone of the model-selection redesign. Every provider exposes a
// *sparse ladder* of these tiers → concrete model ids; routing picks a TIER
// (anchor ± difficulty offset), then resolves the tier to a model per provider.
// Adding a new model becomes a config edit, not a code change.
// ============================================================================

/// Provider-neutral capability tier, ordered least → most capable.
///
/// `Ord` follows declaration order (`Cheapest < CostEfficient < Standard <
/// Frontier`), so the anchor window and sparse-ladder clamp compare and step
/// tiers directly.
///
/// Unlike the legacy substring tier classification it replaced, tier
/// membership is **config exact-match**: a model belongs to whichever tier the
/// provider's `tiers` map points at it. Substring matching is structurally dead
/// (`claude-fable-5` contains no `"opus"`/`"sonnet"`/`"haiku"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityTier {
    /// Fastest / cheapest rung.
    Cheapest,
    /// Cost-optimized rung.
    CostEfficient,
    /// Balanced default rung.
    Standard,
    /// Highest-capability rung.
    Frontier,
}

impl CapabilityTier {
    /// All tiers ascending. Array index is the tier's ladder position.
    pub const ALL: [CapabilityTier; 4] = [
        CapabilityTier::Cheapest,
        CapabilityTier::CostEfficient,
        CapabilityTier::Standard,
        CapabilityTier::Frontier,
    ];

    /// Kebab-case wire form used as the JSON `tiers` map key. MUST stay exact —
    /// `"cost-efficient"`, never `"cost_efficient"` — it is the literal key an
    /// operator types in `.task-mgr/config.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            CapabilityTier::Cheapest => "cheapest",
            CapabilityTier::CostEfficient => "cost-efficient",
            CapabilityTier::Standard => "standard",
            CapabilityTier::Frontier => "frontier",
        }
    }

    /// Strict parse of a config tier key.
    ///
    /// Trims + lowercases, then matches the exact kebab-case set. Every other
    /// input — typos (`"cost_efficient"`, `"fronteir"`), legacy Claude-family
    /// aliases (`"opus"`/`"sonnet"`/`"haiku"`), the empty string — is a CONFIG
    /// ERROR naming the accepted set, never a silent fall-through.
    pub fn parse(s: &str) -> Result<CapabilityTier, String> {
        let trimmed = s.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "cheapest" => Ok(CapabilityTier::Cheapest),
            "cost-efficient" => Ok(CapabilityTier::CostEfficient),
            "standard" => Ok(CapabilityTier::Standard),
            "frontier" => Ok(CapabilityTier::Frontier),
            _ => Err(format!(
                "unknown capability tier {trimmed:?} (expected one of: \
                 cheapest, cost-efficient, standard, frontier)"
            )),
        }
    }

    /// Ladder index (`Cheapest = 0 … Frontier = 3`).
    fn index(self) -> usize {
        CapabilityTier::ALL
            .iter()
            .position(|t| *t == self)
            .expect("ALL contains every CapabilityTier variant")
    }

    /// Tier at ladder `index`, or `None` when out of range.
    fn from_index(index: usize) -> Option<CapabilityTier> {
        CapabilityTier::ALL.get(index).copied()
    }
}

/// Difficulty offset applied to the anchor tier: `low → −1`, `medium → 0`,
/// `high → +1`; absent / unknown difficulty contributes `0`.
///
/// Derived from [`difficulty_rank`] (`low=0, medium=1, high=2`) as `rank − 1`,
/// so the difficulty ladder and the offset window share ONE source of truth.
fn difficulty_offset(difficulty: Option<&str>) -> isize {
    match difficulty_rank(difficulty) {
        Some(rank) => rank as isize - 1,
        None => 0,
    }
}

/// Select a capability tier from the anchor and task difficulty, clamped to the
/// ladder ends so the offset NEVER wraps.
///
/// Single source of truth for anchor/tier derivation (PRD §2.6): the spawn-site
/// resolver and every recovery path call it, so a recovering task derives the
/// exact tier it was first routed by. With `anchor=standard`:
/// `low→cost-efficient`, `medium→standard`, `high→frontier`. At the ends,
/// `anchor=cheapest`+`low` saturates at `cheapest` and `anchor=frontier`+`high`
/// saturates at `frontier` — never wrapping around the ladder.
pub fn anchored_tier(anchor: CapabilityTier, difficulty: Option<&str>) -> CapabilityTier {
    let target = anchor.index() as isize + difficulty_offset(difficulty);
    let last = CapabilityTier::ALL.len() as isize - 1;
    let clamped = target.clamp(0, last) as usize;
    CapabilityTier::from_index(clamped).unwrap_or(anchor)
}

/// Strip a trailing 1M-context suffix ([`ONE_M_SUFFIX`], case-insensitive) for
/// tier reverse lookup. `claude-fable-5[1m]` → `claude-fable-5`. Model ids are
/// ASCII so the byte slice lands on a char boundary.
fn strip_1m_suffix(model: &str) -> &str {
    let n = ONE_M_SUFFIX.len();
    if model.len() >= n && model[model.len() - n..].eq_ignore_ascii_case(ONE_M_SUFFIX) {
        &model[..model.len() - n]
    } else {
        model
    }
}

/// The fully-resolved execution decision for one task: which provider, which
/// concrete model (`None` = spawn with no `-m`/`--model` flag — the provider's
/// own default), which capability tier it landed on, and which effort level to
/// pass. Produced by `resolve_execution_plan` (FEAT-004); this is the struct it
/// returns and both prompt builders thread to the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub provider: Provider,
    pub model: Option<String>,
    pub tier: CapabilityTier,
    pub effort: Option<String>,
}

/// One provider's resolved capability ladder + effort table + routing metadata,
/// built once per run from a merged [`ProviderConfig`](crate::loop_engine::project_config::ProviderConfig).
#[derive(Debug, Clone, PartialEq)]
struct ResolvedProvider {
    enabled: bool,
    /// Sparse tier → model map. A present key is a *defined rung* (a valid
    /// clamp target); its value is `Some(model)` or `None` (route with no model
    /// flag). An absent key is an *undefined rung*, skipped by clamping.
    tiers: BTreeMap<CapabilityTier, Option<String>>,
    /// Normalized difficulty → effort level. `None` value = no effort flag.
    effort: HashMap<String, Option<String>>,
    fallback: Option<Provider>,
    cli_binary: Option<String>,
}

/// Typed, resolved view of the `models` + `routing` config, built ONCE per run.
/// After construction all lookups are cheap, allocation-free reads.
///
/// `model_for` / `tier_of` / `effort_for` are the keystone lookups every
/// downstream task consumes; `routing` is carried for the resolution chain
/// (FEAT-004). Construction is [`resolve_models_config`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModelsConfig {
    pub primary_provider: Provider,
    pub anchor: CapabilityTier,
    providers: HashMap<Provider, ResolvedProvider>,
    pub routing: RoutingConfig,
}

/// Build the typed [`ResolvedModelsConfig`] from a merged `models` block and its
/// `routing` block.
///
/// Infallible: malformed provider/tier/difficulty keys (already rejected by
/// [`validate_models_config`](crate::loop_engine::project_config::validate_models_config))
/// are skipped rather than erroring, so resolution is a pure typed projection of
/// already-validated input.
pub fn resolve_models_config(
    models: &ModelsConfig,
    routing: &RoutingConfig,
) -> ResolvedModelsConfig {
    let mut providers = HashMap::new();
    for (name, pcfg) in &models.providers {
        let Ok(provider) = parse_config_provider(name) else {
            continue;
        };
        let mut tiers = BTreeMap::new();
        for (tier_key, model) in &pcfg.tiers {
            if let Ok(tier) = CapabilityTier::parse(tier_key) {
                tiers.insert(tier, model.clone());
            }
        }
        let mut effort = HashMap::new();
        for (difficulty, level) in &pcfg.effort {
            if let Some(norm) = normalize_difficulty(Some(difficulty)) {
                effort.insert(norm, level.clone());
            }
        }
        let fallback = pcfg
            .fallback
            .as_deref()
            .and_then(|f| parse_config_provider(f).ok());
        providers.insert(
            provider,
            ResolvedProvider {
                enabled: pcfg.enabled,
                tiers,
                effort,
                fallback,
                cli_binary: pcfg.cli_binary.clone(),
            },
        );
    }
    let primary_provider =
        parse_config_provider(&models.primary_provider).unwrap_or(Provider::Claude);
    let anchor = CapabilityTier::parse(&models.anchor).unwrap_or(CapabilityTier::Standard);
    ResolvedModelsConfig {
        primary_provider,
        anchor,
        providers,
        routing: routing.clone(),
    }
}

impl ResolvedModelsConfig {
    /// Concrete model for `(provider, tier)` with sparse-ladder clamping.
    ///
    /// Finds the nearest *defined* rung to `tier` — distance 0 first, then each
    /// successive distance checking DOWN before UP (so an equidistant tie
    /// resolves to the cheaper tier) — and returns that rung's value. `None`
    /// means "route with no model flag": either the nearest defined rung holds
    /// a `null` value, or the provider has no defined rung at all (or is absent
    /// from the config entirely).
    pub fn model_for(&self, provider: Provider, tier: CapabilityTier) -> Option<&str> {
        let p = self.providers.get(&provider)?;
        let target = tier.index() as isize;
        let len = CapabilityTier::ALL.len() as isize;
        for d in 0..len {
            for cand in [target - d, target + d] {
                if (0..len).contains(&cand) {
                    let t = CapabilityTier::from_index(cand as usize)
                        .expect("cand bounded to ladder range");
                    if let Some(value) = p.tiers.get(&t) {
                        return value.as_deref();
                    }
                }
                if d == 0 {
                    break; // distance 0 is a single tier; do not check it twice
                }
            }
        }
        None
    }

    /// Reverse lookup: which tier does `model` belong to for `provider`?
    ///
    /// Strips a trailing [`ONE_M_SUFFIX`] before an EXACT config match (so
    /// `claude-fable-5[1m]` → `Frontier`). Substring matching is dead. Returns
    /// `None` when the provider is absent or no tier maps to the model.
    /// `validate_models_config` guarantees each model maps to at most one tier,
    /// so the match is unambiguous.
    pub fn tier_of(&self, provider: Provider, model: &str) -> Option<CapabilityTier> {
        let base = strip_1m_suffix(model);
        let p = self.providers.get(&provider)?;
        p.tiers
            .iter()
            .find(|(_, value)| value.as_deref() == Some(base))
            .map(|(tier, _)| *tier)
    }

    /// Effort level for `(provider, difficulty)` from the provider's table.
    ///
    /// Reuses the shared [`normalize_difficulty`] — there is NO second
    /// difficulty normalizer. `None` = no effort flag (absent difficulty,
    /// unknown difficulty, absent provider, or an explicit `null` table entry).
    pub fn effort_for(&self, provider: Provider, difficulty: Option<&str>) -> Option<&str> {
        let p = self.providers.get(&provider)?;
        let norm = normalize_difficulty(difficulty)?;
        p.effort.get(&norm)?.as_deref()
    }

    /// Enabled providers paired with their configured CLI-binary override, for
    /// the binary probe. Resolution methods never touch the filesystem;
    /// `probe_enabled_provider_binaries` consumes this to do the I/O separately.
    pub fn enabled_providers(&self) -> Vec<(Provider, Option<&str>)> {
        let mut out: Vec<(Provider, Option<&str>)> = self
            .providers
            .iter()
            .filter(|(_, p)| p.enabled)
            .map(|(provider, p)| (*provider, p.cli_binary.as_deref()))
            .collect();
        // Deterministic order (HashMap iteration is unspecified) so probe
        // failures are reproducible.
        out.sort_by_key(|(provider, _)| provider.as_str());
        out
    }

    /// The configured tier-preserving fallback provider for `provider`, if any.
    /// Used by the cross-provider recovery rung (FEAT-007).
    pub fn fallback_provider(&self, provider: Provider) -> Option<Provider> {
        self.providers.get(&provider).and_then(|p| p.fallback)
    }

    /// Whether `provider` is present in the config AND enabled. A provider
    /// absent from the config is treated as disabled (fail-closed) — class
    /// provider-preference and blackout reroute only ever select an enabled
    /// provider.
    pub fn is_provider_enabled(&self, provider: Provider) -> bool {
        self.providers.get(&provider).is_some_and(|p| p.enabled)
    }
}

// ============================================================================
// Task classification + the FR-003 six-rung resolution chain (FEAT-004)
//
// `classify_task` is the classification SSoT consumed by BOTH the class-route
// rung AND spillover eligibility. `resolve_execution_plan` is the single pure
// resolver both prompt builders call — it NEVER writes `tasks.model`
// (escape-valve contract); escalation/promotion paths in recovery.rs remain
// the only writers.
// ============================================================================

/// Semantic task class. Maps a task ID to a `routing.taskClasses` config key
/// and (for [`TaskClass::Review`]) a built-in, non-redefinable frontier force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    /// Built-in review class ([`is_frontier_class`]). Forces the frontier tier;
    /// NOT redefinable via config and never spillover-eligible.
    Review,
    /// Planning-phase work ([`PLANNING_PREFIXES`]). Routed via
    /// `routing.taskClasses.planning` when present.
    Planning,
    /// Default class — every ID that is neither review nor planning. Routed via
    /// `routing.taskClasses.implementation` when present, else the anchor window.
    Implementation,
}

impl TaskClass {
    /// The `routing.taskClasses` map key for this class.
    pub fn config_key(self) -> &'static str {
        match self {
            TaskClass::Review => "review",
            TaskClass::Planning => "planning",
            TaskClass::Implementation => "implementation",
        }
    }
}

/// Planning-phase task-ID prefixes (`SPIKE-…`, `PLAN-…`). Matched with the same
/// dash-boundary [`id_body_matches_prefix`] used everywhere else — never substring.
pub const PLANNING_PREFIXES: &[&str] = &["SPIKE", "PLAN"];

/// Default implementation-class prefixes. This list MUST remain a SUPERSET of
/// `commands::next::selection::SPAWNED_FIXUP_PREFIXES` so spawned fixup tasks
/// (`CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, `REFACTOR-N-`) classify as
/// implementation rather than drifting into another class. A consistency test
/// pins the subset relationship. Unmatched IDs also fall through to
/// [`TaskClass::Implementation`]; this list exists to document the intent and
/// anchor that guard.
pub const IMPLEMENTATION_PREFIXES: &[&str] = &[
    "FEAT",
    "FIX",
    "REFACTOR",
    "REFACTOR-N",
    "CODE-FIX",
    "WIRE-FIX",
    "IMPL-FIX",
    "TEST",
    "VERIFY",
    "CONTRACT",
];

/// Classify a task ID into a [`TaskClass`] — the classification SSoT.
///
/// Order: built-in review ([`is_frontier_class`], not redefinable) → planning
/// prefixes → implementation (every other ID, the default). Built on
/// [`id_body_matches_prefix`] (dash-boundary token matching after stripping the
/// 8-hex project prefix), never substring. The `routing` parameter is accepted
/// for signature stability with the rung-3 consumer and future config-driven
/// classification; classification today is purely ID-prefix based.
pub fn classify_task(id: &str, _routing: &RoutingConfig) -> TaskClass {
    if is_frontier_class(id) {
        return TaskClass::Review;
    }
    let body = strip_prd_prefix(id);
    if PLANNING_PREFIXES
        .iter()
        .any(|p| id_body_matches_prefix(body, p))
    {
        return TaskClass::Planning;
    }
    TaskClass::Implementation
}

/// Inputs to [`resolve_execution_plan`]. Built per resolution pass; the
/// `provider_blackouts` set is derived per pass and NEVER stored (FEAT-008
/// populates it from account-level quota signals; FEAT-004 callers pass an
/// empty set).
pub struct PlanContext<'a> {
    /// Task ID, used for `byIdPrefix` routing and classification.
    pub task_id: &'a str,
    /// Explicit `tasks.model` for the task (empty/whitespace normalized to None).
    pub task_model: Option<&'a str>,
    /// Task difficulty (DB value, lowercase). Drives the anchor window offset
    /// and the per-provider effort lookup.
    pub difficulty: Option<&'a str>,
    /// The resolved provider-first config (tiers, anchor, routing).
    pub models: &'a ResolvedModelsConfig,
    /// Providers under a quota blackout this pass. Spillover-eligible default
    /// tasks reroute off these; explicit/byIdPrefix/class-forced routes do not.
    pub provider_blackouts: &'a HashSet<Provider>,
}

/// The FR-003 six-rung resolution chain as a single pure function. Both prompt
/// builders (`prompt::sequential` + `prompt::slot`) call this — there is no
/// second resolution path on the spawn side.
///
/// Rungs (named, not numbered — the deferred cascade PRD inserts a rung between
/// EXPLICIT_MODEL and BY_ID_PREFIX):
///
/// - **EXPLICIT_MODEL** — `tasks.model` set → provider via [`provider_for_model`],
///   tier via [`ResolvedModelsConfig::tier_of`] (anchor window fallback if the
///   model is off-ladder), effort from that provider's table.
/// - **BY_ID_PREFIX** — `routing.byIdPrefix` forced route (provider + optional
///   forced tier). Beats the class route, blackout reroute, and anchor window.
/// - **TASK_CLASS** — [`classify_task`] → `routing.taskClasses` provider
///   preference + forced tier; review forces frontier (built-in). Beats the
///   blackout reroute and anchor window.
/// - **QUOTA_BLACKOUT** — when the provider was chosen by the default path (not
///   forced above) and is blacked out AND the task is spillover-eligible,
///   reroute (tier-preserving) to an enabled, non-blacked-out provider. Beats
///   the anchor window's provider choice; never touched by `runner_overrides`.
/// - **ANCHOR_WINDOW** — the default tier: [`anchored_tier`] (low → anchor−1,
///   medium → anchor, high → anchor+1, clamped).
/// - **TIER_TO_MODEL** — [`ResolvedModelsConfig::model_for`] resolves the FINAL
///   (provider, tier) to a concrete model; effort is taken LAST from the FINAL
///   provider's effort table.
///
/// NEVER writes `tasks.model` — pure, no I/O, no DB.
pub fn resolve_execution_plan(plan: &PlanContext<'_>) -> ExecutionPlan {
    let models = plan.models;
    let routing = &models.routing;

    // Rung EXPLICIT_MODEL.
    if let Some(m) = normalize(plan.task_model) {
        let provider = provider_for_model(Some(m));
        let tier = models
            .tier_of(provider, m)
            .unwrap_or_else(|| anchored_tier(models.anchor, plan.difficulty));
        return ExecutionPlan {
            provider,
            model: Some(m.to_string()),
            tier,
            effort: models
                .effort_for(provider, plan.difficulty)
                .map(str::to_string),
        };
    }

    // Rung BY_ID_PREFIX — a forced route beats class / blackout / anchor.
    if let Some((provider, tier)) = byidprefix_route(models, plan.task_id, plan.difficulty) {
        return finalize_plan(models, provider, tier, plan.difficulty);
    }

    // Rung TASK_CLASS — when the class produces a forced provider and/or tier,
    // it beats the blackout reroute and the anchor window.
    let class = classify_task(plan.task_id, routing);
    if let Some((provider, tier)) = class_route(models, class, plan.difficulty) {
        return finalize_plan(models, provider, tier, plan.difficulty);
    }

    // Default path: primary provider on the anchor window, eligible for the
    // QUOTA_BLACKOUT reroute.
    let tier = anchored_tier(models.anchor, plan.difficulty);
    let provider = reroute_for_blackout(models, models.primary_provider, plan, class);
    finalize_plan(models, provider, tier, plan.difficulty)
}

/// Resolve `(provider, tier)` for a `routing.byIdPrefix` match, or `None`.
/// The forced tier is the route's `tier` (validated) when present, else the
/// anchor window for the task's difficulty.
fn byidprefix_route(
    models: &ResolvedModelsConfig,
    task_id: &str,
    difficulty: Option<&str>,
) -> Option<(Provider, CapabilityTier)> {
    let body = strip_prd_prefix(task_id);
    let route: &RouteSpec = models
        .routing
        .by_id_prefix
        .iter()
        .find(|(prefix, _)| id_body_matches_prefix(body, prefix))
        .map(|(_, spec)| spec)?;
    let provider = parse_config_provider(&route.provider).unwrap_or(models.primary_provider);
    let tier = route
        .tier
        .as_deref()
        .and_then(|t| CapabilityTier::parse(t).ok())
        .unwrap_or_else(|| anchored_tier(models.anchor, difficulty));
    Some((provider, tier))
}

/// Resolve `(provider, tier)` for a task class, or `None` when the class does
/// not force a route (the default-path tasks that fall through to the anchor
/// window + blackout reroute).
///
/// Review forces the frontier tier built-in (not redefinable). For every class
/// the provider preference comes from the config route's `byDifficulty` override
/// (matched via the shared [`normalize_difficulty`]) then `providerPreference`;
/// the first ENABLED provider wins.
fn class_route(
    models: &ResolvedModelsConfig,
    class: TaskClass,
    difficulty: Option<&str>,
) -> Option<(Provider, CapabilityTier)> {
    let route = models.routing.task_classes.get(class.config_key());

    let forced_tier = if class == TaskClass::Review {
        // Built-in, not redefinable: review always forces frontier.
        Some(CapabilityTier::Frontier)
    } else {
        route
            .and_then(|r| r.force_tier.as_deref())
            .and_then(|t| CapabilityTier::parse(t).ok())
    };

    let forced_provider = route.and_then(|r| class_provider(models, r, difficulty));

    match (forced_provider, forced_tier) {
        (None, None) => None,
        (provider, tier) => Some((
            provider.unwrap_or(models.primary_provider),
            tier.unwrap_or_else(|| anchored_tier(models.anchor, difficulty)),
        )),
    }
}

/// First enabled provider for a class route: `byDifficulty[difficulty]` (matched
/// lowercase-trimmed via [`normalize_difficulty`]) then `providerPreference`.
fn class_provider(
    models: &ResolvedModelsConfig,
    route: &TaskClassRoute,
    difficulty: Option<&str>,
) -> Option<Provider> {
    if let Some(norm) = normalize_difficulty(difficulty) {
        let by_difficulty = route
            .by_difficulty
            .iter()
            .find(|(k, _)| normalize_difficulty(Some(k)).as_deref() == Some(norm.as_str()))
            .map(|(_, names)| names);
        if let Some(names) = by_difficulty
            && let Some(p) = first_enabled_provider(models, names)
        {
            return Some(p);
        }
    }
    first_enabled_provider(models, &route.provider_preference)
}

/// First entry in `names` that parses to a [`Provider`] and is enabled.
fn first_enabled_provider(models: &ResolvedModelsConfig, names: &[String]) -> Option<Provider> {
    names
        .iter()
        .filter_map(|n| parse_config_provider(n).ok())
        .find(|p| models.is_provider_enabled(*p))
}

/// Spillover eligibility (FR-008), consuming [`classify_task`]'s result so it
/// and the rung-3 class route agree. A task may reroute off a blacked-out
/// provider only when: it is NOT the built-in review class (review is
/// frontier-critical and waits for quota rather than dropping tier), AND its
/// difficulty is at or below `routing.spillover.maxDifficulty`. `maxDifficulty`
/// unset → spillover disabled. Absent/unranked difficulty is treated as the
/// lowest rung (eligible).
fn is_spillover_eligible(
    class: TaskClass,
    difficulty: Option<&str>,
    spillover: &SpilloverConfig,
) -> bool {
    if class == TaskClass::Review {
        return false;
    }
    let Some(max) = spillover.max_difficulty.as_deref() else {
        return false;
    };
    let Some(max_rank) = difficulty_rank(Some(max)) else {
        return false;
    };
    match difficulty_rank(difficulty) {
        Some(rank) => rank <= max_rank,
        None => true,
    }
}

/// Rung QUOTA_BLACKOUT: reroute the default-path provider off a quota blackout.
/// Returns the original provider unchanged when it is not blacked out, the task
/// is not spillover-eligible, or no enabled non-blacked-out alternative exists
/// (FEAT-008's deferral-first path then waits for the quota reset). The
/// reroute is tier-preserving and EPHEMERAL — it never reads or writes
/// `runner_overrides` (the permanent-promotion channel owned by `promote_once`).
fn reroute_for_blackout(
    models: &ResolvedModelsConfig,
    provider: Provider,
    plan: &PlanContext<'_>,
    class: TaskClass,
) -> Provider {
    if !plan.provider_blackouts.contains(&provider) {
        return provider;
    }
    if !is_spillover_eligible(class, plan.difficulty, &models.routing.spillover) {
        return provider;
    }
    // Prefer the configured tier-preserving fallback when it is enabled and not
    // itself blacked out; otherwise the first enabled, non-blacked-out provider.
    if let Some(fb) = models.fallback_provider(provider)
        && models.is_provider_enabled(fb)
        && !plan.provider_blackouts.contains(&fb)
    {
        return fb;
    }
    models
        .enabled_providers()
        .into_iter()
        .map(|(p, _)| p)
        .find(|p| *p != provider && !plan.provider_blackouts.contains(p))
        .unwrap_or(provider)
}

/// Rung TIER_TO_MODEL: resolve the FINAL `(provider, tier)` to a concrete model
/// and read effort LAST from the FINAL provider's table.
fn finalize_plan(
    models: &ResolvedModelsConfig,
    provider: Provider,
    tier: CapabilityTier,
    difficulty: Option<&str>,
) -> ExecutionPlan {
    ExecutionPlan {
        provider,
        model: models.model_for(provider, tier).map(str::to_string),
        tier,
        effort: models.effort_for(provider, difficulty).map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_body_prefix_matching_keeps_dash_segment_boundary() {
        assert!(id_body_matches_prefix("REVIEW-001", "REVIEW"));
        assert!(id_body_matches_prefix("MILESTONE-FINAL", "MILESTONE-FINAL"));
        assert!(!id_body_matches_prefix("REVIEWER-001", "REVIEW"));
    }

    // ============ final_provider_hint tests (REFACTOR-009) ============
    //
    // Pins the shared post-plan drop rule both dispatch sites route through:
    // preserve on a None→default widening (provider-only Codex), drop on a
    // genuine model rewrite. A change to either path's widening handling that
    // re-introduces the blanket-drop bug fails here.

    #[test]
    fn final_hint_preserved_on_codex_widening() {
        // resolved_model None (Codex `model:""`), effective widened to default.
        assert_eq!(
            final_provider_hint(
                Some(Provider::Codex),
                Some(OPUS_MODEL),
                None,
                Some(OPUS_MODEL),
            ),
            Some(Provider::Codex),
        );
    }

    #[test]
    fn final_hint_preserved_when_unchanged() {
        // No escalation: effective == resolved, hint flows straight through.
        assert_eq!(
            final_provider_hint(
                Some(Provider::Grok),
                Some("grok-build"),
                Some("grok-build"),
                Some(OPUS_MODEL),
            ),
            Some(Provider::Grok),
        );
    }

    #[test]
    fn final_hint_dropped_on_rewrite() {
        // Crash escalation rewrote the model to a different provider's id —
        // the stale hint must be dropped or it would mis-route the runner.
        assert_eq!(
            final_provider_hint(
                Some(Provider::Grok),
                Some(FABLE_MODEL),
                Some("grok-build"),
                Some(OPUS_MODEL),
            ),
            None,
        );
    }

    #[test]
    fn final_hint_none_both_none_no_default_kept() {
        // Sequential's un-widened Codex case: effective and resolved both None,
        // equal → hint preserved even though it never reached `default_model`.
        assert_eq!(
            final_provider_hint(Some(Provider::Codex), None, None, Some(OPUS_MODEL)),
            Some(Provider::Codex),
        );
    }

    // ============ escalate_tier tests ============
    //
    // The builtin Claude ladder is config exact-match (NO substring matching):
    // Cheapest=haiku, CostEfficient=sonnet, Standard=opus, Frontier=fable.
    // `escalate_tier` self-loops at the Frontier ceiling; off-ladder ids → None.

    #[test]
    fn test_escalate_tier_haiku_to_sonnet() {
        let r = escalate_tier(
            builtin_resolved_models(),
            Provider::Claude,
            Some(HAIKU_MODEL),
        );
        assert_eq!(r, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_tier_sonnet_to_opus() {
        let r = escalate_tier(
            builtin_resolved_models(),
            Provider::Claude,
            Some(SONNET_MODEL),
        );
        assert_eq!(r, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_tier_opus_to_fable() {
        // opus is now the Standard rung; Frontier (fable) sits above it.
        let r = escalate_tier(
            builtin_resolved_models(),
            Provider::Claude,
            Some(OPUS_MODEL),
        );
        assert_eq!(r, Some(FABLE_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_tier_fable_self_loops_at_ceiling() {
        // Frontier is the ceiling: self-loop, NO wrap, NO provider hop.
        let r = escalate_tier(
            builtin_resolved_models(),
            Provider::Claude,
            Some(FABLE_MODEL),
        );
        assert_eq!(r, Some(FABLE_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_tier_none_stays_none() {
        assert_eq!(
            escalate_tier(builtin_resolved_models(), Provider::Claude, None),
            None
        );
    }

    #[test]
    fn test_escalate_tier_off_ladder_returns_none() {
        // Exact-match only: an id not on the provider's ladder can't escalate.
        for m in ["gpt-4", "", "  ", "llama-3-70b", "totally-unknown-xyz"] {
            assert_eq!(
                escalate_tier(builtin_resolved_models(), Provider::Claude, Some(m)),
                None,
                "off-ladder id {m:?} must return None (no substring fallback)",
            );
        }
    }

    #[test]
    fn test_escalate_tier_grok_single_rung_self_loops() {
        // Grok has one defined rung (Standard=grok-build); escalation self-loops.
        let r = escalate_tier(
            builtin_resolved_models(),
            Provider::Grok,
            Some("grok-build"),
        );
        assert_eq!(r, Some("grok-build".to_string()));
    }

    /// Full chain haiku → sonnet → opus → fable → fable (ceiling self-loop).
    #[test]
    fn test_escalate_tier_full_chain() {
        let resolved = builtin_resolved_models();
        let a = escalate_tier(resolved, Provider::Claude, Some(HAIKU_MODEL));
        assert_eq!(a, Some(SONNET_MODEL.to_string()));
        let b = escalate_tier(resolved, Provider::Claude, a.as_deref());
        assert_eq!(b, Some(OPUS_MODEL.to_string()));
        let c = escalate_tier(resolved, Provider::Claude, b.as_deref());
        assert_eq!(c, Some(FABLE_MODEL.to_string()));
        let d = escalate_tier(resolved, Provider::Claude, c.as_deref());
        assert_eq!(d, Some(FABLE_MODEL.to_string()), "fable is the ceiling");
    }

    // ============ downgrade_effort tests ============

    #[test]
    fn test_downgrade_effort_xhigh_to_high() {
        assert_eq!(downgrade_effort(Some("xhigh")), Some("high"));
    }

    #[test]
    fn test_downgrade_effort_high_is_floor() {
        assert_eq!(
            downgrade_effort(Some("high")),
            None,
            "high is the floor: must not downgrade to medium"
        );
    }

    #[test]
    fn test_downgrade_effort_medium_and_low_return_none() {
        assert_eq!(downgrade_effort(Some("medium")), None);
        assert_eq!(downgrade_effort(Some("low")), None);
    }

    #[test]
    fn test_downgrade_effort_none_returns_none() {
        assert_eq!(downgrade_effort(None), None);
    }

    #[test]
    fn test_downgrade_effort_unknown_returns_none() {
        assert_eq!(downgrade_effort(Some("")), None);
        assert_eq!(downgrade_effort(Some("max")), None);
        assert_eq!(downgrade_effort(Some("ultra")), None);
        assert_eq!(downgrade_effort(Some("HIGH")), None, "case-sensitive");
    }

    // ============ EFFORT_FOR_DIFFICULTY invariants ============

    #[test]
    fn test_effort_for_difficulty_new_mapping() {
        assert_eq!(
            EFFORT_FOR_DIFFICULTY,
            &[("low", "medium"), ("medium", "high"), ("high", "xhigh"),],
            "difficulty→effort ladder must be medium/high/xhigh (max retired)"
        );
    }

    #[test]
    fn test_effort_for_difficulty_never_produces_max() {
        for (_, effort) in EFFORT_FOR_DIFFICULTY {
            assert_ne!(
                *effort, "max",
                "max effort is retired — no difficulty should map to it"
            );
        }
    }

    // ============ effort_for_difficulty tests ============

    #[test]
    fn test_effort_for_difficulty_roundtrips_table() {
        for (difficulty, expected) in EFFORT_FOR_DIFFICULTY {
            assert_eq!(effort_for_difficulty(Some(difficulty)), Some(*expected));
            assert_eq!(
                effort_for_difficulty(Some(&difficulty.to_ascii_uppercase())),
                Some(*expected),
                "lookup must be case-insensitive"
            );
        }
    }

    #[test]
    fn test_effort_for_difficulty_unknown_and_none() {
        assert_eq!(effort_for_difficulty(None), None);
        assert_eq!(effort_for_difficulty(Some("")), None);
        assert_eq!(effort_for_difficulty(Some("   ")), None);
        assert_eq!(effort_for_difficulty(Some("impossible")), None);
    }

    /// Pins the shared `normalize_difficulty` contract via its two public
    /// callers: both must trim whitespace and lowercase before lookup so DB
    /// values like `"  High  "` resolve identically to `"high"`.
    #[test]
    fn test_difficulty_normalization_trims_and_lowercases() {
        for raw in [" low ", "LOW", "\tlow\n", "  Low  "] {
            assert_eq!(
                effort_for_difficulty(Some(raw)),
                effort_for_difficulty(Some("low")),
                "effort lookup must normalize {raw:?}"
            );
            assert_eq!(
                difficulty_rank(Some(raw)),
                difficulty_rank(Some("low")),
                "rank lookup must normalize {raw:?}"
            );
        }
    }

    // ============ difficulty_rank tests ============

    /// The rank order is derived from the table's index, so the assertion
    /// walks the table rather than hardcoding 0/1/2.
    #[test]
    fn test_difficulty_rank_matches_table_index() {
        for (i, (difficulty, _)) in EFFORT_FOR_DIFFICULTY.iter().enumerate() {
            assert_eq!(difficulty_rank(Some(difficulty)), Some(i));
        }
    }

    #[test]
    fn test_difficulty_rank_case_insensitive() {
        for mutation in ["HIGH", "High", "hIgH"] {
            assert_eq!(
                difficulty_rank(Some(mutation)),
                difficulty_rank(Some("high")),
                "{mutation} must rank the same as 'high'"
            );
        }
    }

    #[test]
    fn test_difficulty_rank_unknown_and_empty() {
        assert_eq!(difficulty_rank(None), None);
        assert_eq!(difficulty_rank(Some("")), None);
        assert_eq!(difficulty_rank(Some("   ")), None);
        assert_eq!(difficulty_rank(Some("trivial")), None);
        assert_eq!(difficulty_rank(Some("impossible")), None);
    }

    #[test]
    fn test_difficulty_rank_ascending() {
        // Ladder must be strictly ascending so cluster max is well-defined.
        let low = difficulty_rank(Some("low")).unwrap();
        let medium = difficulty_rank(Some("medium")).unwrap();
        let high = difficulty_rank(Some("high")).unwrap();
        assert!(low < medium && medium < high);
    }

    // ============ is_1m_model tests ============

    #[test]
    fn test_is_1m_model_positive() {
        assert!(is_1m_model(Some(&format!("{OPUS_MODEL}{ONE_M_SUFFIX}"))));
        assert!(is_1m_model(Some("claude-opus-4-7[1m]")));
        assert!(is_1m_model(Some("anything[1M]")));
        assert!(is_1m_model(Some("model[1m]suffix")));
    }

    #[test]
    fn test_is_1m_model_negative() {
        assert!(!is_1m_model(None));
        assert!(!is_1m_model(Some(OPUS_MODEL)));
        assert!(!is_1m_model(Some(SONNET_MODEL)));
        assert!(!is_1m_model(Some("")));
        assert!(!is_1m_model(Some("opus")));
    }

    // ============ to_1m_model tests ============
    //
    // Suffix-append, Claude-only (per `provider_for_model`). Covers EVERY Claude
    // tier including the new frontier (fable) — not just Opus.

    #[test]
    fn test_to_1m_model_opus_returns_1m() {
        assert_eq!(
            to_1m_model(Some(OPUS_MODEL)),
            Some(format!("{OPUS_MODEL}{ONE_M_SUFFIX}"))
        );
    }

    #[test]
    fn test_to_1m_model_fable_returns_1m() {
        // The AC-mandated case: claude-fable-5 → claude-fable-5[1m].
        assert_eq!(
            to_1m_model(Some(FABLE_MODEL)),
            Some(format!("{FABLE_MODEL}{ONE_M_SUFFIX}")),
        );
    }

    #[test]
    fn test_to_1m_model_any_claude_tier_gets_suffix() {
        // No Opus gate: suffix-append fires on any Claude-classified model.
        assert_eq!(
            to_1m_model(Some(SONNET_MODEL)),
            Some(format!("{SONNET_MODEL}{ONE_M_SUFFIX}")),
        );
        assert_eq!(
            to_1m_model(Some(HAIKU_MODEL)),
            Some(format!("{HAIKU_MODEL}{ONE_M_SUFFIX}")),
        );
    }

    #[test]
    fn test_to_1m_model_already_1m_returns_none() {
        assert_eq!(
            to_1m_model(Some(&format!("{OPUS_MODEL}{ONE_M_SUFFIX}"))),
            None
        );
        assert_eq!(
            to_1m_model(Some(&format!("{FABLE_MODEL}{ONE_M_SUFFIX}"))),
            None
        );
    }

    #[test]
    fn test_to_1m_model_grok_returns_none() {
        // 1M context is a Claude-only capability — Grok has no variant.
        assert_eq!(to_1m_model(Some("grok-build")), None);
        assert_eq!(to_1m_model(Some("grok-code-fast-1")), None);
    }

    #[test]
    fn test_to_1m_model_none_and_blank_return_none() {
        assert_eq!(to_1m_model(None), None);
        assert_eq!(to_1m_model(Some("")), None);
        assert_eq!(to_1m_model(Some("   ")), None);
    }

    // ============ is_frontier_class tests ============

    #[test]
    fn test_is_frontier_class_positive_unprefixed() {
        assert!(is_frontier_class("CODE-REVIEW-1"));
        assert!(is_frontier_class("MILESTONE-FINAL"));
        assert!(is_frontier_class("REVIEW-001"));
        assert!(is_frontier_class("CODE-REVIEW-007"));
        assert!(is_frontier_class("REVIEW-FINAL"));
    }

    #[test]
    fn test_is_frontier_class_positive_prefixed() {
        // Regression guard: bare starts_with("CODE-REVIEW-") would fail this.
        assert!(
            is_frontier_class("8d71d1f7-CODE-REVIEW-1"),
            "prefixed CODE-REVIEW-1 must be true"
        );
        assert!(is_frontier_class("8d71d1f7-MILESTONE-FINAL"));
        assert!(is_frontier_class("8d71d1f7-REVIEW-001"));
    }

    #[test]
    fn test_is_frontier_class_negative_unprefixed() {
        assert!(!is_frontier_class("REFACTOR-REVIEW-FINAL"));
        assert!(!is_frontier_class("MILESTONE-1"));
        assert!(!is_frontier_class("MILESTONE-2"));
        assert!(!is_frontier_class("VERIFY-001"));
        assert!(!is_frontier_class("REFACTOR-001"));
        assert!(!is_frontier_class("FEAT-001"));
    }

    #[test]
    fn test_is_frontier_class_negative_prefixed() {
        // Stripping first '-' (wrong) would turn REFACTOR-REVIEW-FINAL into
        // REVIEW-FINAL and falsely match REVIEW-; strip only 8-hex-char prefix.
        assert!(
            !is_frontier_class("8d71d1f7-REFACTOR-REVIEW-FINAL"),
            "prefixed REFACTOR-REVIEW-FINAL must be false"
        );
        assert!(!is_frontier_class("8d71d1f7-CODE-FIX-001"));
        assert!(!is_frontier_class("8d71d1f7-FEAT-001"));
        assert!(!is_frontier_class("8d71d1f7-MILESTONE-1"));
    }

    #[test]
    fn test_is_frontier_class_non_hex_prefix_not_stripped() {
        // "CODE-REV" starts with non-hex chars; must NOT be treated as a prefix.
        // id = "CODE-REV-CODE-REVIEW-1": body stays as-is because first 8 chars
        // include uppercase which is outside [0-9a-f].
        assert!(!is_frontier_class("CODE-REV-CODE-REVIEW-1"));
    }

    #[test]
    fn test_is_frontier_class_uppercase_hex_prefix_not_stripped() {
        // Uppercase hex like "ABCDEF01" is outside [0-9a-f]; must not strip.
        assert!(!is_frontier_class("ABCDEF01-CODE-REVIEW-1"));
    }

    // ============ escalate_below_ceiling tests ============
    //
    // Same one-tier-up walk as escalate_tier but returns None at the ceiling
    // (Frontier) so the overflow ladder can fall through to the 1M rung.

    #[test]
    fn test_escalate_below_ceiling_haiku_to_sonnet() {
        assert_eq!(
            escalate_below_ceiling(
                builtin_resolved_models(),
                Provider::Claude,
                Some(HAIKU_MODEL)
            ),
            Some(SONNET_MODEL.to_string()),
        );
    }

    #[test]
    fn test_escalate_below_ceiling_sonnet_to_opus() {
        assert_eq!(
            escalate_below_ceiling(
                builtin_resolved_models(),
                Provider::Claude,
                Some(SONNET_MODEL)
            ),
            Some(OPUS_MODEL.to_string()),
        );
    }

    #[test]
    fn test_escalate_below_ceiling_opus_to_fable() {
        // opus (Standard) still has Frontier (fable) above it — not the ceiling.
        assert_eq!(
            escalate_below_ceiling(
                builtin_resolved_models(),
                Provider::Claude,
                Some(OPUS_MODEL)
            ),
            Some(FABLE_MODEL.to_string()),
        );
    }

    #[test]
    fn test_escalate_below_ceiling_fable_is_ceiling() {
        assert_eq!(
            escalate_below_ceiling(
                builtin_resolved_models(),
                Provider::Claude,
                Some(FABLE_MODEL)
            ),
            None,
            "fable is the Frontier ceiling; the 1M rung handles the rest",
        );
    }

    #[test]
    fn test_escalate_below_ceiling_fable_1m_is_ceiling() {
        // tier_of strips [1m] → fable → Frontier → no higher tier.
        let fable_1m = format!("{FABLE_MODEL}{ONE_M_SUFFIX}");
        assert_eq!(
            escalate_below_ceiling(builtin_resolved_models(), Provider::Claude, Some(&fable_1m)),
            None,
        );
    }

    #[test]
    fn test_escalate_below_ceiling_grok_single_rung_is_ceiling() {
        // Grok's single rung IS its ceiling — no model escalation, no provider hop.
        assert_eq!(
            escalate_below_ceiling(
                builtin_resolved_models(),
                Provider::Grok,
                Some("grok-build")
            ),
            None,
        );
    }

    #[test]
    fn test_escalate_below_ceiling_off_ladder_returns_none() {
        assert_eq!(
            escalate_below_ceiling(builtin_resolved_models(), Provider::Claude, Some("gpt-4")),
            None,
        );
        assert_eq!(
            escalate_below_ceiling(builtin_resolved_models(), Provider::Claude, Some("")),
            None,
        );
        assert_eq!(
            escalate_below_ceiling(builtin_resolved_models(), Provider::Claude, None),
            None,
        );
    }

    // ============ parse_config_provider tests (FEAT-006) ============
    //
    // Hard-fails on unknown providers so a config typo (`"openai"`,
    // `"codex-cli"`, `"groq"`) surfaces as a validation error instead of
    // silently routing the task to Claude. The "Known-bad" AC: an
    // Option-returning parser silently routes a typo to Claude — these
    // tests assert the typo produces an Err.

    #[test]
    fn test_parse_config_provider_canonical_lowercase() {
        assert_eq!(parse_config_provider("claude"), Ok(Provider::Claude));
        assert_eq!(parse_config_provider("grok"), Ok(Provider::Grok));
        assert_eq!(parse_config_provider("codex"), Ok(Provider::Codex));
    }

    #[test]
    fn test_parse_config_provider_trim_and_case_insensitive() {
        assert_eq!(parse_config_provider("  CODEX "), Ok(Provider::Codex));
        assert_eq!(parse_config_provider("\tGrok\n"), Ok(Provider::Grok));
        assert_eq!(parse_config_provider("CLAUDE"), Ok(Provider::Claude));
        assert_eq!(parse_config_provider("CoDeX"), Ok(Provider::Codex));
    }

    #[test]
    fn test_parse_config_provider_rejects_lookalikes_and_unknowns() {
        // Vendor look-alikes that an Option-returning parser would silently
        // drop to None (and therefore route to Claude). The strict variant
        // surfaces the error.
        assert!(parse_config_provider("openai").is_err());
        assert!(parse_config_provider("codex-cli").is_err());
        assert!(parse_config_provider("groq").is_err()); // Groq Inc. ≠ xAI Grok
        assert!(parse_config_provider("anthropic").is_err());
        assert!(parse_config_provider("gpt").is_err());
        assert!(parse_config_provider("").is_err());
        assert!(parse_config_provider("   ").is_err());
        let err = parse_config_provider("openai").unwrap_err();
        assert!(
            err.contains("openai")
                && err.contains("claude")
                && err.contains("grok")
                && err.contains("codex"),
            "error message must name the rejected value and the allowed set: {err:?}",
        );
    }

    #[test]
    fn test_parse_config_provider_round_trips_through_as_str() {
        // Contract: every Provider value round-trips losslessly through
        // `as_str → parse_config_provider`. The accepted set is EXACTLY the
        // `as_str` image.
        for p in [Provider::Claude, Provider::Grok, Provider::Codex] {
            assert_eq!(
                parse_config_provider(p.as_str()),
                Ok(p),
                "round-trip through as_str → parse_config_provider must be lossless for {p:?}",
            );
        }
    }

    // ============ CapabilityTier + ResolvedModelsConfig (FR-001) ============

    use crate::loop_engine::project_config::{ModelsConfig, ProviderConfig, RoutingConfig};

    /// Build a production-shaped single-provider `ResolvedModelsConfig` from a
    /// sparse tier ladder. Uses the real `ProviderConfig`/`ModelsConfig` structs
    /// through `resolve_models_config` — NOT a hand-rolled map — so the tests
    /// exercise the same path the deserializer feeds.
    fn resolved_single(
        provider: Provider,
        tiers: &[(CapabilityTier, Option<&str>)],
    ) -> ResolvedModelsConfig {
        let mut providers = HashMap::new();
        providers.insert(
            provider.as_str().to_string(),
            ProviderConfig {
                enabled: true,
                tiers: tiers
                    .iter()
                    .map(|(t, m)| (t.as_str().to_string(), m.map(str::to_string)))
                    .collect(),
                effort: HashMap::new(),
                fallback: None,
                cli_binary: None,
            },
        );
        let models = ModelsConfig {
            primary_provider: provider.as_str().to_string(),
            anchor: CapabilityTier::Standard.as_str().to_string(),
            providers,
        };
        resolve_models_config(&models, &RoutingConfig::default())
    }

    #[test]
    fn capability_tier_as_str_is_exact_kebab_case() {
        assert_eq!(CapabilityTier::Cheapest.as_str(), "cheapest");
        assert_eq!(CapabilityTier::CostEfficient.as_str(), "cost-efficient");
        assert_eq!(CapabilityTier::Standard.as_str(), "standard");
        assert_eq!(CapabilityTier::Frontier.as_str(), "frontier");
        // The footgun this pins: snake_case must NEVER leak into the wire form.
        assert_ne!(CapabilityTier::CostEfficient.as_str(), "cost_efficient");
    }

    #[test]
    fn capability_tier_parse_accepts_canonical_and_round_trips() {
        for tier in CapabilityTier::ALL {
            assert_eq!(CapabilityTier::parse(tier.as_str()), Ok(tier));
        }
        // Trim + case-insensitive.
        assert_eq!(
            CapabilityTier::parse("  COST-EFFICIENT \n"),
            Ok(CapabilityTier::CostEfficient)
        );
    }

    #[test]
    fn capability_tier_parse_rejects_typos_and_legacy_aliases() {
        // Legacy Claude-family aliases must NOT be accepted as tier keys.
        for bad in ["opus", "sonnet", "haiku", "cost_efficient", "fronteir", ""] {
            let err = CapabilityTier::parse(bad).unwrap_err();
            assert!(
                err.contains("cheapest")
                    && err.contains("cost-efficient")
                    && err.contains("standard")
                    && err.contains("frontier"),
                "error for {bad:?} must name the accepted set; got {err:?}"
            );
        }
    }

    #[test]
    fn capability_tier_is_ordered_ascending() {
        assert!(CapabilityTier::Cheapest < CapabilityTier::CostEfficient);
        assert!(CapabilityTier::CostEfficient < CapabilityTier::Standard);
        assert!(CapabilityTier::Standard < CapabilityTier::Frontier);
    }

    #[test]
    fn capability_tier_model_for_full_ladder_exact() {
        let r = resolved_single(
            Provider::Claude,
            &[
                (CapabilityTier::Cheapest, Some(HAIKU_MODEL)),
                (CapabilityTier::CostEfficient, Some(SONNET_MODEL)),
                (CapabilityTier::Standard, Some(OPUS_MODEL)),
                (CapabilityTier::Frontier, Some(FABLE_MODEL)),
            ],
        );
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Cheapest),
            Some(HAIKU_MODEL)
        );
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::CostEfficient),
            Some(SONNET_MODEL)
        );
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Standard),
            Some(OPUS_MODEL)
        );
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Frontier),
            Some(FABLE_MODEL)
        );
    }

    #[test]
    fn capability_tier_model_for_single_tier_ladder_clamps_every_request() {
        // Only Standard defined → EVERY requested tier clamps to it.
        let r = resolved_single(
            Provider::Claude,
            &[(CapabilityTier::Standard, Some(OPUS_MODEL))],
        );
        for tier in CapabilityTier::ALL {
            assert_eq!(
                r.model_for(Provider::Claude, tier),
                Some(OPUS_MODEL),
                "tier {tier:?} should clamp to the only defined rung"
            );
        }
    }

    #[test]
    fn capability_tier_model_for_gap_ladder_nearest_down_first() {
        // Only cheapest + frontier defined (the gap-ladder case).
        let r = resolved_single(
            Provider::Claude,
            &[
                (CapabilityTier::Cheapest, Some(HAIKU_MODEL)),
                (CapabilityTier::Frontier, Some(FABLE_MODEL)),
            ],
        );
        // Exact rungs.
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Cheapest),
            Some(HAIKU_MODEL)
        );
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Frontier),
            Some(FABLE_MODEL)
        );
        // CostEfficient: cheapest(dist 1) beats frontier(dist 2) → down wins.
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::CostEfficient),
            Some(HAIKU_MODEL)
        );
        // Standard: frontier(dist 1) beats cheapest(dist 2) → nearest is up.
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Standard),
            Some(FABLE_MODEL)
        );
    }

    #[test]
    fn capability_tier_model_for_null_value_routes_with_no_flag() {
        // A defined rung whose value is null = route-with-no-model-flag.
        let r = resolved_single(Provider::Claude, &[(CapabilityTier::Standard, None)]);
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Standard),
            None,
            "null value at the defined rung → None (no -m flag)"
        );
        // Frontier clamps down to the only defined (null) rung → still None.
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Frontier),
            None
        );
    }

    #[test]
    fn capability_tier_model_for_absent_provider_is_none() {
        let r = resolved_single(
            Provider::Claude,
            &[(CapabilityTier::Standard, Some(OPUS_MODEL))],
        );
        assert_eq!(r.model_for(Provider::Grok, CapabilityTier::Standard), None);
    }

    #[test]
    fn capability_tier_tier_of_exact_match_and_1m_strip() {
        let r = resolved_single(
            Provider::Claude,
            &[
                (CapabilityTier::Cheapest, Some(HAIKU_MODEL)),
                (CapabilityTier::Standard, Some(OPUS_MODEL)),
                (CapabilityTier::Frontier, Some(FABLE_MODEL)),
            ],
        );
        assert_eq!(
            r.tier_of(Provider::Claude, OPUS_MODEL),
            Some(CapabilityTier::Standard)
        );
        assert_eq!(
            r.tier_of(Provider::Claude, HAIKU_MODEL),
            Some(CapabilityTier::Cheapest)
        );
        // [1m] suffix stripped before the exact match.
        assert_eq!(
            r.tier_of(Provider::Claude, "claude-fable-5[1m]"),
            Some(CapabilityTier::Frontier)
        );
        assert_eq!(
            r.tier_of(Provider::Claude, &format!("{OPUS_MODEL}{ONE_M_SUFFIX}")),
            Some(CapabilityTier::Standard)
        );
        // Unknown model / wrong provider → None (no substring fallback).
        assert_eq!(r.tier_of(Provider::Claude, "gpt-4"), None);
        assert_eq!(r.tier_of(Provider::Grok, OPUS_MODEL), None);
    }

    #[test]
    fn capability_tier_effort_for_uses_shared_normalizer() {
        let mut providers = HashMap::new();
        providers.insert(
            Provider::Claude.as_str().to_string(),
            ProviderConfig {
                enabled: true,
                tiers: HashMap::from([("standard".to_string(), Some(OPUS_MODEL.to_string()))]),
                effort: HashMap::from([
                    ("low".to_string(), Some("medium".to_string())),
                    ("high".to_string(), None), // explicit null = no flag
                ]),
                fallback: None,
                cli_binary: None,
            },
        );
        let models = ModelsConfig {
            primary_provider: "claude".to_string(),
            anchor: "standard".to_string(),
            providers,
        };
        let r = resolve_models_config(&models, &RoutingConfig::default());
        // Normalized lookup (trim + lowercase) via the shared normalize_difficulty.
        assert_eq!(
            r.effort_for(Provider::Claude, Some("  LOW ")),
            Some("medium")
        );
        // Explicit null entry → None.
        assert_eq!(r.effort_for(Provider::Claude, Some("high")), None);
        // Unknown difficulty / None / absent provider → None.
        assert_eq!(r.effort_for(Provider::Claude, Some("medium")), None);
        assert_eq!(r.effort_for(Provider::Claude, None), None);
        assert_eq!(r.effort_for(Provider::Grok, Some("low")), None);
    }

    #[test]
    fn capability_tier_anchored_tier_window_and_end_clamps() {
        use CapabilityTier::*;
        // anchor=standard window.
        assert_eq!(anchored_tier(Standard, Some("low")), CostEfficient);
        assert_eq!(anchored_tier(Standard, Some("medium")), Standard);
        assert_eq!(anchored_tier(Standard, Some("high")), Frontier);
        // Absent / unknown difficulty → anchor itself (offset 0).
        assert_eq!(anchored_tier(Standard, None), Standard);
        assert_eq!(anchored_tier(Standard, Some("trivial")), Standard);
        // End clamps: offset never wraps.
        assert_eq!(
            anchored_tier(Cheapest, Some("low")),
            Cheapest,
            "cheapest−1 clamps"
        );
        assert_eq!(anchored_tier(Cheapest, Some("high")), CostEfficient);
        assert_eq!(
            anchored_tier(Frontier, Some("high")),
            Frontier,
            "frontier+1 clamps"
        );
        assert_eq!(anchored_tier(Frontier, Some("low")), Standard);
    }

    #[test]
    fn capability_tier_default_anchor_standard_sanity() {
        // PRD success metric: default config + anchor=standard →
        // low=sonnet, medium=opus, high=fable.
        let r = resolve_models_config(&ModelsConfig::builtin_default(), &RoutingConfig::default());
        assert_eq!(r.anchor, CapabilityTier::Standard);
        for (difficulty, expected) in [
            ("low", SONNET_MODEL),
            ("medium", OPUS_MODEL),
            ("high", FABLE_MODEL),
        ] {
            let tier = anchored_tier(r.anchor, Some(difficulty));
            assert_eq!(
                r.model_for(Provider::Claude, tier),
                Some(expected),
                "anchor=standard, difficulty={difficulty}"
            );
        }
    }

    #[test]
    fn capability_tier_anchor_cost_efficient_sanity() {
        // anchor=cost-efficient → low=haiku, medium=sonnet, high=opus.
        let mut models = ModelsConfig::builtin_default();
        models.anchor = CapabilityTier::CostEfficient.as_str().to_string();
        let r = resolve_models_config(&models, &RoutingConfig::default());
        for (difficulty, expected) in [
            ("low", HAIKU_MODEL),
            ("medium", SONNET_MODEL),
            ("high", OPUS_MODEL),
        ] {
            let tier = anchored_tier(r.anchor, Some(difficulty));
            assert_eq!(
                r.model_for(Provider::Claude, tier),
                Some(expected),
                "anchor=cost-efficient, difficulty={difficulty}"
            );
        }
    }

    #[test]
    fn capability_tier_grok_enable_is_complete_optin_via_field_merge() {
        // {"providers":{"grok":{"enabled":true}}} must be a COMPLETE opt-in:
        // grok inherits the default ladder + effort table, just flips enabled.
        let user = serde_json::json!({ "providers": { "grok": { "enabled": true } } });
        let merged = crate::loop_engine::project_config::merge_models_config(Some(&user)).unwrap();
        let grok = merged
            .providers
            .get("grok")
            .expect("grok present after merge");
        assert!(grok.enabled, "user override flips enabled");
        assert_eq!(
            grok.tiers.get("standard"),
            Some(&Some("grok-build".to_string())),
            "grok inherits its default tier ladder"
        );
        assert!(
            !grok.effort.is_empty(),
            "grok inherits its default effort table"
        );
        // Untouched providers keep their defaults.
        assert!(merged.providers.get("claude").unwrap().enabled);
        assert!(!merged.providers.get("codex").unwrap().enabled);
    }

    #[test]
    fn capability_tier_resolve_skips_unparseable_keys() {
        // resolve is a typed projection of VALIDATED input; defensively it
        // skips a junk provider/tier key rather than panicking.
        let mut providers = HashMap::new();
        providers.insert(
            "claude".to_string(),
            ProviderConfig {
                enabled: true,
                tiers: HashMap::from([
                    ("standard".to_string(), Some(OPUS_MODEL.to_string())),
                    ("bogus-tier".to_string(), Some("x".to_string())),
                ]),
                effort: HashMap::new(),
                fallback: None,
                cli_binary: None,
            },
        );
        providers.insert(
            "openai".to_string(), // unparseable provider key
            ProviderConfig::default(),
        );
        let models = ModelsConfig {
            primary_provider: "claude".to_string(),
            anchor: "standard".to_string(),
            providers,
        };
        let r = resolve_models_config(&models, &RoutingConfig::default());
        assert_eq!(
            r.model_for(Provider::Claude, CapabilityTier::Standard),
            Some(OPUS_MODEL)
        );
        // The bogus tier key never created a rung.
        assert_eq!(r.tier_of(Provider::Claude, "x"), None);
    }

    // ============ classify_task / is_frontier_class (FEAT-004) ============

    #[test]
    fn classify_task_review_class_is_built_in() {
        let routing = RoutingConfig::default();
        assert_eq!(classify_task("CODE-REVIEW-1", &routing), TaskClass::Review);
        assert_eq!(
            classify_task("MILESTONE-FINAL", &routing),
            TaskClass::Review
        );
        assert_eq!(classify_task("REVIEW-007", &routing), TaskClass::Review);
        // 8-hex project prefix is stripped before matching.
        assert_eq!(
            classify_task("8d71d1f7-CODE-REVIEW-1", &routing),
            TaskClass::Review
        );
        // REFACTOR-REVIEW is explicitly excluded from the review class.
        assert_eq!(
            classify_task("REFACTOR-REVIEW-1", &routing),
            TaskClass::Implementation
        );
    }

    #[test]
    fn classify_task_planning_and_implementation_defaults() {
        let routing = RoutingConfig::default();
        assert_eq!(classify_task("SPIKE-1", &routing), TaskClass::Planning);
        assert_eq!(classify_task("PLAN-2", &routing), TaskClass::Planning);
        assert_eq!(
            classify_task("FEAT-001", &routing),
            TaskClass::Implementation
        );
        assert_eq!(
            classify_task("CODE-FIX-9", &routing),
            TaskClass::Implementation
        );
        // Unmatched IDs fall through to the implementation default.
        assert_eq!(
            classify_task("WHATEVER-1", &routing),
            TaskClass::Implementation
        );
    }

    /// AC #6: `SPAWNED_FIXUP_PREFIXES` MUST be a subset of the implementation
    /// class defaults so spawned fixups never drift into another class.
    #[test]
    fn spawned_fixup_prefixes_subset_of_implementation_defaults() {
        use crate::commands::next::selection::SPAWNED_FIXUP_PREFIXES;
        for prefix in SPAWNED_FIXUP_PREFIXES {
            assert!(
                IMPLEMENTATION_PREFIXES.contains(prefix),
                "SPAWNED_FIXUP prefix {prefix:?} must be in IMPLEMENTATION_PREFIXES \
                 (prefix sets drifted)"
            );
        }
    }

    // ============ resolve_execution_plan (FR-003 six rungs) ============

    /// A multi-provider config in the production JSON shape: claude full ladder,
    /// grok single `standard` rung, codex single `standard` rung (null model),
    /// all enabled. `anchor` and `routing` are caller-supplied.
    fn plan_models(anchor: CapabilityTier, routing: serde_json::Value) -> ResolvedModelsConfig {
        let models: ModelsConfig = serde_json::from_value(serde_json::json!({
            "primaryProvider": "claude",
            "anchor": anchor.as_str(),
            "providers": {
                "claude": {
                    "enabled": true,
                    "tiers": {
                        "cheapest": HAIKU_MODEL,
                        "cost-efficient": SONNET_MODEL,
                        "standard": OPUS_MODEL,
                        "frontier": FABLE_MODEL,
                    },
                    "effort": { "low": "medium", "medium": "high", "high": "xhigh" },
                    "fallback": "grok",
                },
                "grok": {
                    "enabled": true,
                    "tiers": { "standard": "grok-build" },
                    "effort": { "low": "low", "medium": "medium", "high": "high" },
                },
                "codex": {
                    "enabled": true,
                    "tiers": { "standard": null },
                    "effort": { "low": "low", "medium": "medium", "high": "high" },
                },
            }
        }))
        .expect("production-shaped models fixture");
        let routing: RoutingConfig =
            serde_json::from_value(routing).expect("production-shaped routing fixture");
        resolve_models_config(&models, &routing)
    }

    fn plan(
        models: &ResolvedModelsConfig,
        id: &str,
        task_model: Option<&str>,
        difficulty: Option<&str>,
        blackouts: &HashSet<Provider>,
    ) -> ExecutionPlan {
        resolve_execution_plan(&PlanContext {
            task_id: id,
            task_model,
            difficulty,
            models,
            provider_blackouts: blackouts,
        })
    }

    /// US-004 / PRD success metric: default config + anchor=standard maps the
    /// difficulty window to sonnet / opus / fable, all on Claude. High-difficulty
    /// implementation stays on Claude (frontier = claude-fable-5).
    #[test]
    fn default_anchor_standard_difficulty_window_stays_on_claude() {
        let models = plan_models(CapabilityTier::Standard, serde_json::json!({}));
        let no_blackout = HashSet::new();

        let low = plan(&models, "FEAT-1", None, Some("low"), &no_blackout);
        assert_eq!(low.provider, Provider::Claude);
        assert_eq!(low.model.as_deref(), Some(SONNET_MODEL));
        assert_eq!(low.tier, CapabilityTier::CostEfficient);

        let med = plan(&models, "FEAT-2", None, Some("medium"), &no_blackout);
        assert_eq!(med.provider, Provider::Claude);
        assert_eq!(med.model.as_deref(), Some(OPUS_MODEL));
        assert_eq!(med.tier, CapabilityTier::Standard);

        let high = plan(&models, "FEAT-3", None, Some("high"), &no_blackout);
        assert_eq!(high.provider, Provider::Claude);
        assert_eq!(high.model.as_deref(), Some(FABLE_MODEL));
        assert_eq!(high.tier, CapabilityTier::Frontier);
        // Effort comes LAST from the final (Claude) provider's table.
        assert_eq!(high.effort.as_deref(), Some("xhigh"));
    }

    /// anchor=cost-efficient slides the whole window down one rung.
    #[test]
    fn anchor_cost_efficient_window() {
        let models = plan_models(CapabilityTier::CostEfficient, serde_json::json!({}));
        let nb = HashSet::new();
        assert_eq!(
            plan(&models, "FEAT-1", None, Some("low"), &nb)
                .model
                .as_deref(),
            Some(HAIKU_MODEL)
        );
        assert_eq!(
            plan(&models, "FEAT-2", None, Some("medium"), &nb)
                .model
                .as_deref(),
            Some(SONNET_MODEL)
        );
        assert_eq!(
            plan(&models, "FEAT-3", None, Some("high"), &nb)
                .model
                .as_deref(),
            Some(OPUS_MODEL)
        );
    }

    /// AC #2 precedence: explicit `tasks.model` beats a byIdPrefix route.
    #[test]
    fn precedence_explicit_beats_byidprefix() {
        let routing = serde_json::json!({
            "byIdPrefix": { "FEAT": { "provider": "grok" } }
        });
        let models = plan_models(CapabilityTier::Standard, routing);
        let p = plan(
            &models,
            "FEAT-1",
            Some(HAIKU_MODEL),
            Some("high"),
            &HashSet::new(),
        );
        assert_eq!(p.provider, Provider::Claude);
        assert_eq!(p.model.as_deref(), Some(HAIKU_MODEL));
        assert_eq!(p.tier, CapabilityTier::Cheapest); // tier_of(haiku)
    }

    /// AC #2 precedence: a byIdPrefix route beats the class route.
    #[test]
    fn precedence_byidprefix_beats_class_route() {
        let routing = serde_json::json!({
            "byIdPrefix": { "FEAT": { "provider": "grok" } },
            "taskClasses": { "implementation": { "providerPreference": ["codex"] } }
        });
        let models = plan_models(CapabilityTier::Standard, routing);
        let p = plan(&models, "FEAT-1", None, Some("medium"), &HashSet::new());
        assert_eq!(p.provider, Provider::Grok);
        assert_eq!(p.model.as_deref(), Some("grok-build"));
    }

    /// AC #2 precedence: a class route beats the blackout reroute (the forced
    /// provider stays even under a blackout — it is deferred, not rerouted).
    #[test]
    fn precedence_class_route_beats_blackout_reroute() {
        let routing = serde_json::json!({
            "taskClasses": { "implementation": { "providerPreference": ["claude"] } },
            "spillover": { "maxDifficulty": "high" }
        });
        let models = plan_models(CapabilityTier::Standard, routing);
        let mut blackout = HashSet::new();
        blackout.insert(Provider::Claude);
        let p = plan(&models, "FEAT-1", None, Some("medium"), &blackout);
        assert_eq!(p.provider, Provider::Claude);
    }

    /// AC #2 precedence: the blackout reroute beats the plain anchor window —
    /// a default-path task on a blacked-out primary reroutes (tier preserved).
    #[test]
    fn precedence_blackout_reroute_beats_anchor_window() {
        let routing = serde_json::json!({ "spillover": { "maxDifficulty": "high" } });
        let models = plan_models(CapabilityTier::Standard, routing);
        let mut blackout = HashSet::new();
        blackout.insert(Provider::Claude);
        let p = plan(&models, "FEAT-1", None, Some("medium"), &blackout);
        assert_eq!(p.provider, Provider::Grok);
        assert_eq!(p.model.as_deref(), Some("grok-build"));
    }

    #[test]
    fn anchor_window_is_the_floor_when_nothing_else_fires() {
        let models = plan_models(CapabilityTier::Standard, serde_json::json!({}));
        let p = plan(&models, "FEAT-1", None, Some("high"), &HashSet::new());
        assert_eq!(p.provider, Provider::Claude);
        assert_eq!(p.tier, CapabilityTier::Frontier);
    }

    /// Review class forces the frontier tier built-in even with an empty routing
    /// config (not redefinable), and never reroutes under a blackout.
    #[test]
    fn review_class_forces_frontier_built_in() {
        let models = plan_models(CapabilityTier::Cheapest, serde_json::json!({}));
        let p = plan(&models, "CODE-REVIEW-1", None, Some("low"), &HashSet::new());
        assert_eq!(p.tier, CapabilityTier::Frontier);
        assert_eq!(p.provider, Provider::Claude);
        assert_eq!(p.model.as_deref(), Some(FABLE_MODEL));

        let mut blackout = HashSet::new();
        blackout.insert(Provider::Claude);
        let routing = serde_json::json!({ "spillover": { "maxDifficulty": "high" } });
        let models = plan_models(CapabilityTier::Standard, routing);
        let p = plan(&models, "CODE-REVIEW-2", None, Some("high"), &blackout);
        assert_eq!(p.provider, Provider::Claude);
    }

    /// AC #7: `byDifficulty` keys + the lookup difficulty are both matched
    /// lowercase-trimmed via the shared `normalize_difficulty` — no second
    /// normalizer. A `"High"` map key matches a `" high "` task difficulty.
    #[test]
    fn class_by_difficulty_matched_via_shared_normalizer() {
        let routing = serde_json::json!({
            "taskClasses": {
                "implementation": {
                    "providerPreference": ["claude"],
                    "byDifficulty": { "High": ["grok"] }
                }
            }
        });
        let models = plan_models(CapabilityTier::Standard, routing);
        let p = plan(&models, "FEAT-1", None, Some(" high "), &HashSet::new());
        assert_eq!(
            p.provider,
            Provider::Grok,
            "byDifficulty 'High' must match difficulty ' high ' via normalize_difficulty"
        );
    }

    /// Codex routes resolve a `None` model (provider-only spawn) and read effort
    /// from the codex table (capped at high).
    #[test]
    fn codex_route_resolves_null_model_and_codex_effort() {
        let routing = serde_json::json!({
            "byIdPrefix": { "FEAT": { "provider": "codex" } }
        });
        let models = plan_models(CapabilityTier::Standard, routing);
        let p = plan(&models, "FEAT-1", None, Some("high"), &HashSet::new());
        assert_eq!(p.provider, Provider::Codex);
        assert_eq!(p.model, None);
        assert_eq!(p.effort.as_deref(), Some("high"));
    }

    /// Spillover eligibility consumes `classify_task`: review is never eligible;
    /// implementation is eligible up to `maxDifficulty`; disabled when unset.
    #[test]
    fn spillover_eligibility_respects_class_and_max_difficulty() {
        let spill = SpilloverConfig {
            max_difficulty: Some("medium".to_string()),
            ..SpilloverConfig::default()
        };
        assert!(is_spillover_eligible(
            TaskClass::Implementation,
            Some("low"),
            &spill
        ));
        assert!(is_spillover_eligible(
            TaskClass::Implementation,
            Some("medium"),
            &spill
        ));
        assert!(!is_spillover_eligible(
            TaskClass::Implementation,
            Some("high"),
            &spill
        ));
        assert!(!is_spillover_eligible(
            TaskClass::Review,
            Some("low"),
            &spill
        ));
        // Unset maxDifficulty disables spillover entirely.
        let spill_off = SpilloverConfig::default();
        assert!(!is_spillover_eligible(
            TaskClass::Implementation,
            Some("low"),
            &spill_off
        ));
    }
}
