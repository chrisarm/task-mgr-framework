use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::config_io::{OnCorruptJson, write_config_key_at};
use crate::loop_engine::model::{
    CODEX_EFFORT_FOR_DIFFICULTY, CapabilityTier, EFFORT_FOR_DIFFICULTY, FABLE_MODEL, HAIKU_MODEL,
    OPUS_MODEL, Provider, ResolvedModelsConfig, SONNET_MODEL, normalize_route_prefix,
    parse_baseline_tier_key, parse_config_provider, route_prefixes_overlap,
};

/// Configuration for the Grok fallback runner (US-005, FR-006).
///
/// When `enabled = true`, the loop engine promotes tasks to the Grok CLI
/// after the Claude overflow ladder is exhausted (rung 4) or after
/// `runtime_error_threshold` consecutive `RuntimeError` rounds. Absent or
/// `enabled = false` → no change to the existing 4-rung ladder.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FallbackRunnerConfig {
    /// Whether the Grok fallback runner is active. Default: `false`.
    #[serde(default)]
    pub enabled: bool,

    /// LLM provider name. Must be `"grok"` for the xAI Grok CLI.
    /// Default: `"grok"`.
    #[serde(default = "default_fallback_provider")]
    pub provider: String,

    /// Grok model ID passed as `--model`. Default: `"grok-build"`.
    #[serde(default = "default_fallback_model")]
    pub model: String,

    /// Absolute path to the Grok CLI binary. When `None`, the binary is
    /// resolved as `"grok"` on the system PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_binary: Option<String>,

    /// Number of consecutive `RuntimeError` rounds on a task before the
    /// Grok fallback hook fires. Default: `2`.
    #[serde(default = "default_fallback_runtime_error_threshold")]
    pub runtime_error_threshold: u32,
}

impl Default for FallbackRunnerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_fallback_provider(),
            model: default_fallback_model(),
            cli_binary: None,
            runtime_error_threshold: default_fallback_runtime_error_threshold(),
        }
    }
}

/// A provider + optional model pair used as a routing target in `PrimaryRunnerConfig`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunnerSpec {
    /// Provider name (e.g. `"grok"`, `"claude"`).
    pub provider: String,
    /// Model identifier passed to the provider CLI (e.g. `"grok-build"`).
    /// Codex v1 may omit this field to route by explicit provider intent only.
    #[serde(default)]
    pub model: String,
    /// Opt-in: when the route's provider is `"codex"` AND this is `true`, a
    /// Codex RUNTIME failure (not auth) promotes the task to the Claude
    /// runner instead of auto-blocking after `runtimeErrorThreshold` rounds.
    /// One-shot per task — once promoted, normal Claude recovery applies and
    /// the task never returns to Codex. Default: `false` (legacy auto-block).
    /// Ignored on non-codex routes — Claude/Grok runners already have their
    /// own cross-provider promotion paths (`fallbackRunner` / `claudeFallbackModel`).
    #[serde(default)]
    pub runtime_error_fallback: bool,
}

/// Per-task-type and per-id-prefix routing for the primary runner.
///
/// Routes specific task types (e.g. `"review"`, `"milestone"`) or ID prefixes
/// (e.g. `"REVIEW-"`, `"MILESTONE-"`) to a non-default runner, while all other
/// tasks continue to use the default Claude model/runner.
///
/// Phase 1: schema + serde only — resolution-chain wiring comes in a later phase.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryRunnerConfig {
    /// Claude model to fall back to when a task routed via `primaryRunner`
    /// experiences a `RuntimeError`. `None` → use the project/task default
    /// Claude model.
    #[serde(default)]
    pub claude_fallback_model: Option<String>,

    /// Number of consecutive `RuntimeError` rounds before falling back to
    /// Claude. Default: `2`.
    #[serde(default = "default_primary_runtime_error_threshold")]
    pub runtime_error_threshold: u32,

    /// Task-type → `RunnerSpec` routing map. Absent key → empty map.
    #[serde(default)]
    pub by_task_type: HashMap<String, RunnerSpec>,

    /// Task-ID-prefix → `RunnerSpec` routing map. Absent key → empty map.
    #[serde(default)]
    pub by_id_prefix: HashMap<String, RunnerSpec>,

    /// Task-ID-prefix → baseline capability tier → `RunnerSpec` routing map.
    ///
    /// Used when a task has no explicit `model`, after its baseline Claude
    /// model is known from difficulty/default resolution and normalized into
    /// a provider-neutral tier. Tier keys are validated by
    /// `model::parse_baseline_tier_key`.
    #[serde(default)]
    pub baseline_tier_routes: HashMap<String, HashMap<String, RunnerSpec>>,
}

impl Default for PrimaryRunnerConfig {
    fn default() -> Self {
        Self {
            claude_fallback_model: None,
            runtime_error_threshold: default_primary_runtime_error_threshold(),
            by_task_type: HashMap::new(),
            by_id_prefix: HashMap::new(),
            baseline_tier_routes: HashMap::new(),
        }
    }
}

// ============================================================================
// Provider-first model config (FR-001): the `models` + `routing` blocks
//
// Replaces the five legacy surfaces (defaultModel, reviewModel, primaryRunner,
// fallbackRunner). CONTRACT-001 defines the serde types + pure validation +
// the (separate, I/O-doing) binary probe. Wiring into `ProjectConfig` and the
// hard-break deletion of the legacy fields is FEAT-002.
// ============================================================================

/// The `models` config block: provider-first capability-tier routing policy.
///
/// Keys mirror the FR-001 canonical JSON. A user config is a SPARSE override
/// merged field-wise onto [`ModelsConfig::builtin_default`] (see
/// [`merge_models_config`]), so `{"providers":{"grok":{"enabled":true}}}` is a
/// complete opt-in — Grok inherits the default ladder + effort table.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfig {
    /// Provider that owns unrouted / anchor-resolved tasks. Default `"claude"`.
    /// Must parse to a [`Provider`] AND be enabled (validated).
    #[serde(default = "default_primary_provider")]
    pub primary_provider: String,
    /// Anchor capability tier; the difficulty window centers here. Default
    /// `"standard"`. Must parse to a `CapabilityTier` (validated).
    #[serde(default = "default_anchor_tier")]
    pub anchor: String,
    /// Lowercase provider name (`"claude"`/`"grok"`/`"codex"`) → its config.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// One provider's capability ladder, per-provider effort table, and routing
/// metadata.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// Whether this provider may be selected. Claude defaults enabled;
    /// grok/codex default disabled.
    #[serde(default)]
    pub enabled: bool,
    /// Kebab-case capability tier (`"cost-efficient"`) → model id. A `null`
    /// value = "route with no model flag"; an absent tier key is undefined.
    #[serde(default)]
    pub tiers: HashMap<String, Option<String>>,
    /// Difficulty (`"low"`/`"medium"`/`"high"`) → effort level. `null` value =
    /// no effort flag. Codex must not map to `"xhigh"` (policy cap; validated).
    #[serde(default)]
    pub effort: HashMap<String, Option<String>>,
    /// Tier-preserving cross-provider fallback target. Must be a DIFFERENT,
    /// enabled provider (validated). `None` = no fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Absolute path to this provider's CLI binary; `None` resolves the bare
    /// name on PATH. Probed when enabled by [`probe_enabled_provider_binaries`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_binary: Option<String>,
}

/// The `routing` config block: role-split + difficulty-spillover policy layered
/// over the anchor window. Consumed by `resolve_execution_plan` (FEAT-004).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoutingConfig {
    /// Task-ID-prefix → forced route (provider + optional forced tier).
    #[serde(default)]
    pub by_id_prefix: HashMap<String, RouteSpec>,
    /// Semantic task class (`"review"`/`"planning"`/`"implementation"`) →
    /// route preferences.
    #[serde(default)]
    pub task_classes: HashMap<String, TaskClassRoute>,
    /// Difficulty-spillover eligibility for quota-aware failover (FR-008).
    #[serde(default)]
    pub spillover: SpilloverConfig,
    /// DEFERRED to `tasks/prd-review-cascade.md`. Captured here ONLY so
    /// [`validate_models_config`] can reject it with a "not yet supported"
    /// note — building the cascade is out of scope for this PRD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_cascade: Option<serde_json::Value>,
}

/// A forced route: a provider, optionally pinned to a capability tier.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RouteSpec {
    /// Provider to route to. Must be enabled (validated).
    pub provider: String,
    /// Optional forced capability tier (overrides the anchor window). Must
    /// parse to a `CapabilityTier` (validated). `None` = use the anchor window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

/// Per-task-class routing: an ordered provider preference, an optional forced
/// tier (e.g. review / planning → frontier), and a per-difficulty override map.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskClassRoute {
    /// Ordered provider preference; the first enabled provider wins.
    #[serde(default)]
    pub provider_preference: Vec<String>,
    /// Forced capability tier for this class. Must parse (validated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_tier: Option<String>,
    /// Difficulty → ordered provider-name override. Matches the Data Flow
    /// Contract: `HashMap<String, Vec<String>>` (difficulty → provider names).
    #[serde(default)]
    pub by_difficulty: HashMap<String, Vec<String>>,
}

/// Difficulty-spillover policy for quota-aware failover (FR-008).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpilloverConfig {
    /// Highest task difficulty eligible to spill to another provider on quota
    /// blackout. `None` = spillover disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_difficulty: Option<String>,
}

fn default_primary_provider() -> String {
    Provider::Claude.as_str().to_string()
}

fn default_anchor_tier() -> String {
    CapabilityTier::Standard.as_str().to_string()
}

/// Build a `tiers` map from typed `(tier, model)` pairs. Keeps model-ID
/// literals confined to `model.rs` — entries reference the constants, never
/// hardcode strings here.
fn tier_map(entries: &[(CapabilityTier, Option<&str>)]) -> HashMap<String, Option<String>> {
    entries
        .iter()
        .map(|(t, m)| (t.as_str().to_string(), m.map(str::to_string)))
        .collect()
}

/// Build an `effort` map from a difficulty→effort table (each value is `Some`).
fn effort_map(entries: &[(&str, &str)]) -> HashMap<String, Option<String>> {
    entries
        .iter()
        .map(|(d, e)| (d.to_string(), Some(e.to_string())))
        .collect()
}

/// Built-in default Claude provider: the full capability ladder, enabled.
fn default_claude_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: true,
        tiers: tier_map(&[
            (CapabilityTier::Cheapest, Some(HAIKU_MODEL)),
            (CapabilityTier::CostEfficient, Some(SONNET_MODEL)),
            (CapabilityTier::Standard, Some(OPUS_MODEL)),
            (CapabilityTier::Frontier, Some(FABLE_MODEL)),
        ]),
        effort: effort_map(EFFORT_FOR_DIFFICULTY),
        fallback: None,
        cli_binary: None,
    }
}

/// Built-in default Grok provider: a single `standard` rung (the only model the
/// grok CLI exposes), disabled. One rung keeps the reverse lookup unambiguous.
fn default_grok_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: false,
        tiers: tier_map(&[(CapabilityTier::Standard, Some("grok-build"))]),
        effort: effort_map(EFFORT_FOR_DIFFICULTY),
        fallback: None,
        cli_binary: None,
    }
}

/// Built-in default Codex provider: one `standard` rung with a `null` model
/// (codex is routed provider-only and spawns with no `-m` flag), disabled.
/// Effort is capped at `high` by the codex table.
fn default_codex_provider() -> ProviderConfig {
    ProviderConfig {
        enabled: false,
        tiers: tier_map(&[(CapabilityTier::Standard, None)]),
        effort: effort_map(CODEX_EFFORT_FOR_DIFFICULTY),
        fallback: None,
        cli_binary: None,
    }
}

impl ModelsConfig {
    /// The built-in default `models` block: Claude enabled across the full
    /// ladder, Grok/Codex present-but-disabled, `anchor=standard`,
    /// `primaryProvider=claude`. A user config is merged field-wise onto this
    /// (see [`merge_models_config`]).
    pub fn builtin_default() -> ModelsConfig {
        ModelsConfig {
            primary_provider: default_primary_provider(),
            anchor: default_anchor_tier(),
            providers: HashMap::from([
                (
                    Provider::Claude.as_str().to_string(),
                    default_claude_provider(),
                ),
                (Provider::Grok.as_str().to_string(), default_grok_provider()),
                (
                    Provider::Codex.as_str().to_string(),
                    default_codex_provider(),
                ),
            ]),
        }
    }
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig::builtin_default()
    }
}

/// A process-wide `&'static` builtin-default `models` block, for callers (and
/// the many prompt-builder test sites) that need a borrow to thread into
/// [`crate::loop_engine::model::resolve_execution_plan`] without owning a local.
/// Production threads the run's ACTUAL `ProjectConfig::models` instead.
pub fn default_models_config() -> &'static ModelsConfig {
    static C: std::sync::OnceLock<ModelsConfig> = std::sync::OnceLock::new();
    C.get_or_init(ModelsConfig::builtin_default)
}

/// The `&'static` empty default `routing` block — companion to
/// [`default_models_config`].
pub fn default_routing_config() -> &'static RoutingConfig {
    static C: std::sync::OnceLock<RoutingConfig> = std::sync::OnceLock::new();
    C.get_or_init(RoutingConfig::default)
}

/// Deep-merge `overlay` onto `base` IN PLACE: two objects merge key-by-key
/// (recursively); every other shape (scalar, array, `null`) replaces wholesale.
/// This is the field-wise merge that makes a sparse user override a complete
/// opt-in — a provider the user only flips `enabled` on keeps its default
/// tier ladder, because the nested `tiers` object is never visited.
fn deep_merge_value(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    use serde_json::Value;
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                deep_merge_value(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// Merge a user-supplied `models` JSON value onto the built-in default and
/// deserialize the result.
///
/// `None` / `Some(null)` → the pure built-in default. Otherwise the override is
/// deep-merged field-wise (see [`deep_merge_value`]) so partial provider
/// overrides inherit every unspecified default. Returns the merged
/// [`ModelsConfig`]; validate it with [`validate_models_config`] before use.
pub fn merge_models_config(user: Option<&serde_json::Value>) -> Result<ModelsConfig, String> {
    let mut base = serde_json::to_value(ModelsConfig::builtin_default())
        .map_err(|e| format!("serializing default models config: {e}"))?;
    if let Some(u) = user.filter(|v| !v.is_null()) {
        deep_merge_value(&mut base, u);
    }
    serde_json::from_value(base).map_err(|e| format!("deserializing merged models config: {e}"))
}

/// Legacy model-config keys removed by this PRD's hard break, in canonical
/// order. Surfaced by [`detect_legacy_model_keys`].
const LEGACY_MODEL_KEYS: &[&str] = &[
    "defaultModel",
    "reviewModel",
    "primaryRunner",
    "fallbackRunner",
];

/// Return which legacy model-config keys appear at the TOP LEVEL of `config`,
/// in canonical order. Empty vec = a clean post-migration config.
///
/// Pure: inspects the already-parsed value, no I/O. FEAT-002 wires this into
/// the loop/batch hard-error preflight and the non-loop one-line warning.
pub fn detect_legacy_model_keys(config: &serde_json::Value) -> Vec<&'static str> {
    match config.as_object() {
        Some(obj) => LEGACY_MODEL_KEYS
            .iter()
            .copied()
            .filter(|k| obj.contains_key(*k))
            .collect(),
        None => Vec::new(),
    }
}

/// The minimal FR-001 `models`/`routing` skeleton printed alongside a
/// legacy-key rejection so the operator sees the replacement shape inline.
const FR_001_SCHEMA_SKELETON: &str = r#"{
  "models": {
    "primaryProvider": "claude",
    "anchor": "standard",
    "providers": { "claude": { "enabled": true } }
  },
  "routing": {}
}"#;

/// Build the hard-break rejection message for a set of present legacy keys.
///
/// Names each offending key, prints the FR-001 replacement skeleton, and points
/// at the migration command. Shared by the loop/batch preflight
/// ([`preflight_validate_and_probe`]) and the interim `models` mutating-verb
/// guard so every entry point speaks with one voice (FR-002 coverage table).
pub fn legacy_model_keys_message(keys: &[&str]) -> String {
    format!(
        "legacy model-config key(s) [{keys}] are no longer supported and must be removed — the \
         provider-first `models`/`routing` config replaces all of \
         defaultModel/reviewModel/primaryRunner/fallbackRunner:\n{skeleton}\n\
         Run `task-mgr models init --force-replace-legacy` to migrate.",
        keys = keys.join(", "),
        skeleton = FR_001_SCHEMA_SKELETON,
    )
}

/// Pure schema + semantic validation of a (merged) `models` + `routing` block.
///
/// **NO I/O** — never probes a binary or touches the filesystem. That is
/// [`probe_enabled_provider_binaries`], a SEPARATE enabled-gated function this
/// validator never calls.
///
/// Returns EVERY error found (not just the first) so an operator fixes the
/// config in one pass. Each message names the offending key and the accepted
/// set / reason. Rejects:
/// - unknown provider keys, unknown / legacy-alias tier keys,
/// - a malformed or disabled `primaryProvider`, a malformed `anchor`,
/// - ambiguous reverse model lookups (two tiers → one model),
/// - codex effort `xhigh` (by policy),
/// - a `fallback` to self or to a disabled / unknown provider,
/// - routes referencing disabled / unknown providers or malformed forced tiers,
/// - the premature `routing.reviewCascade` key (deferred — not yet supported).
pub fn validate_models_config(
    models: &ModelsConfig,
    routing: &RoutingConfig,
) -> Result<(), Vec<String>> {
    use std::collections::HashSet;
    let mut errors: Vec<String> = Vec::new();

    // Pass 1: validate provider keys and collect the enabled set.
    let mut enabled: HashSet<Provider> = HashSet::new();
    for (name, pcfg) in &models.providers {
        match parse_config_provider(name) {
            Ok(provider) => {
                if pcfg.enabled {
                    enabled.insert(provider);
                }
            }
            Err(e) => errors.push(format!(
                "models.providers: invalid provider key {name:?} — {e}"
            )),
        }
    }

    // primaryProvider must parse AND be enabled.
    match parse_config_provider(&models.primary_provider) {
        Ok(p) if enabled.contains(&p) => {}
        Ok(p) => errors.push(format!(
            "models.primaryProvider {:?} is disabled — enable providers.{} or pick an enabled provider",
            models.primary_provider,
            p.as_str()
        )),
        Err(e) => errors.push(format!("models.primaryProvider: {e}")),
    }

    // anchor must parse to a CapabilityTier.
    if let Err(e) = CapabilityTier::parse(&models.anchor) {
        errors.push(format!("models.anchor: {e}"));
    }

    // Pass 2: per-provider tiers / effort / fallback.
    for (name, pcfg) in &models.providers {
        let provider = parse_config_provider(name).ok();

        let mut seen_models: HashMap<&str, &str> = HashMap::new();
        for (tier_key, model) in &pcfg.tiers {
            if let Err(e) = CapabilityTier::parse(tier_key) {
                errors.push(format!("models.providers.{name}.tiers: {e}"));
            }
            if let Some(m) = model.as_deref()
                && let Some(prev) = seen_models.insert(m, tier_key)
            {
                errors.push(format!(
                    "models.providers.{name}.tiers: ambiguous reverse lookup — model {m:?} \
                     is mapped by both {prev:?} and {tier_key:?}; each model must map to at \
                     most one tier"
                ));
            }
        }

        if provider == Some(Provider::Codex) {
            for (difficulty, level) in &pcfg.effort {
                if level
                    .as_deref()
                    .is_some_and(|l| l.trim().eq_ignore_ascii_case("xhigh"))
                {
                    errors.push(format!(
                        "models.providers.{name}.effort.{difficulty}: codex effort \"xhigh\" is \
                         rejected by policy (allowed: low, medium, high)"
                    ));
                }
            }
        }

        if let Some(fb) = pcfg.fallback.as_deref() {
            match parse_config_provider(fb) {
                Ok(target) if provider == Some(target) => errors.push(format!(
                    "models.providers.{name}.fallback: a provider cannot fall back to itself ({fb:?})"
                )),
                Ok(target) if !enabled.contains(&target) => errors.push(format!(
                    "models.providers.{name}.fallback: target {fb:?} is not an enabled provider"
                )),
                Ok(_) => {}
                Err(e) => errors.push(format!("models.providers.{name}.fallback: {e}")),
            }
        }
    }

    // Pass 3: routing routes must reference enabled providers; forced tiers
    // must parse.
    let check_provider_ref =
        |errors: &mut Vec<String>, ctx: String, prov: &str| match parse_config_provider(prov) {
            Ok(p) if enabled.contains(&p) => {}
            Ok(p) => errors.push(format!("{ctx}: provider {:?} is not enabled", p.as_str())),
            Err(e) => errors.push(format!("{ctx}: {e}")),
        };
    let check_tier = |errors: &mut Vec<String>, ctx: String, tier: &str| {
        if let Err(e) = CapabilityTier::parse(tier) {
            errors.push(format!("{ctx}: {e}"));
        }
    };

    for (prefix, route) in &routing.by_id_prefix {
        check_provider_ref(
            &mut errors,
            format!("routing.byIdPrefix.{prefix}.provider"),
            &route.provider,
        );
        if let Some(t) = route.tier.as_deref() {
            check_tier(&mut errors, format!("routing.byIdPrefix.{prefix}.tier"), t);
        }
    }
    for (class, route) in &routing.task_classes {
        for prov in &route.provider_preference {
            check_provider_ref(
                &mut errors,
                format!("routing.taskClasses.{class}.providerPreference"),
                prov,
            );
        }
        if let Some(t) = route.force_tier.as_deref() {
            check_tier(
                &mut errors,
                format!("routing.taskClasses.{class}.forceTier"),
                t,
            );
        }
        for (difficulty, provs) in &route.by_difficulty {
            for prov in provs {
                check_provider_ref(
                    &mut errors,
                    format!("routing.taskClasses.{class}.byDifficulty.{difficulty}"),
                    prov,
                );
            }
        }
    }

    // reviewCascade is deferred to the review-cascade PRD — reject up-front.
    if routing.review_cascade.is_some() {
        errors.push(
            "routing.reviewCascade is not yet supported — the multi-provider review cascade is \
             deferred to tasks/prd-review-cascade.md; remove this key"
                .to_string(),
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Verify the CLI binary of every ENABLED provider resolves to an executable.
///
/// SEPARATE from [`validate_models_config`] and NEVER called by it — validation
/// is pure; this performs PATH / filesystem I/O. Disabled providers are skipped,
/// so a Claude-only config never trips a grok/codex probe. Errors name the
/// missing binary and how to fix it. Providers are probed in a deterministic
/// order so a failure is reproducible.
pub fn probe_enabled_provider_binaries(resolved: &ResolvedModelsConfig) -> TaskMgrResult<()> {
    for (provider, cli_binary) in resolved.enabled_providers() {
        probe_provider_binary(provider, cli_binary).map_err(|binary| TaskMgrError::NotFound {
            resource_type: format!("{} runner binary", provider.as_str()),
            id: format!(
                "{binary} — install the {provider} CLI or set \
                 models.providers.{provider}.cliBinary to an executable path, then retry",
                provider = provider.as_str()
            ),
        })?;
    }
    Ok(())
}

/// Resolve + verify one provider's binary (env var → config override → PATH).
fn probe_provider_binary(provider: Provider, cli_binary: Option<&str>) -> Result<(), String> {
    let (env_var, default_name) = match provider {
        Provider::Claude => ("CLAUDE_BINARY", "claude"),
        Provider::Grok => ("GROK_BINARY", "grok"),
        Provider::Codex => ("CODEX_BINARY", "codex"),
    };
    resolve_and_verify_named_binary(env_var, default_name, cli_binary)
}

/// Resolve a provider binary the same way the runtime runners do: `<ENV_VAR>`
/// when set and non-blank, else `cli_binary` when non-blank, else the bare
/// `default_name` searched on PATH. Empty / whitespace values fall through (the
/// `export VAR=""` footgun). Returns `Err(binary_name)` when nothing executable
/// resolves.
fn resolve_and_verify_named_binary(
    env_var: &str,
    default_name: &str,
    cli_binary: Option<&str>,
) -> Result<(), String> {
    let env_bin = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
    let cli_bin = cli_binary.filter(|v| !v.trim().is_empty());

    let (binary, found) = if let Some(env_bin) = env_bin {
        let exec = is_executable_path(Path::new(&env_bin));
        (env_bin, exec)
    } else if let Some(explicit) = cli_bin {
        (
            explicit.to_string(),
            is_executable_path(Path::new(explicit)),
        )
    } else {
        let found = std::env::var_os("PATH")
            .map(|path_var| {
                std::env::split_paths(&path_var)
                    .any(|dir| is_executable_path(&dir.join(default_name)))
            })
            .unwrap_or(false);
        (default_name.to_string(), found)
    };

    if found { Ok(()) } else { Err(binary) }
}

/// Per-project loop configuration read from `.task-mgr/config.json`.
///
/// Allows projects to extend the default tool allowlist with project-specific
/// tools (e.g., `docker`, `curl`, `./scripts/*`) without modifying the core
/// default. Forward-compatible: unknown fields are silently ignored.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    /// Schema version for forward compatibility.
    #[serde(default = "default_version")]
    #[allow(dead_code)]
    pub version: u32,

    /// Additional tool entries appended to CODING_ALLOWED_TOOLS.
    /// Example: `["Bash(docker:*)", "Bash(curl:*)"]`
    #[serde(default)]
    pub additional_allowed_tools: Vec<String>,

    /// Permission mode override for this project.
    /// Values: `"dangerous"`, `"scoped"`, `"auto"`.
    /// When set, overrides the default `Dangerous` mode (env vars still win).
    /// Set to `"scoped"` or `"auto"` to opt this project back into permission
    /// prompts / allowlist enforcement.
    #[serde(default)]
    pub permission_mode: Option<String>,

    /// Ollama server URL for embedding generation.
    /// Defaults to `http://localhost:11435` (the bundled docker-compose stack
    /// uses 11435 to avoid clashing with a host-installed `ollama serve`).
    #[serde(default)]
    pub ollama_url: Option<String>,

    /// Embedding model name for Ollama.
    /// Defaults to `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0`.
    #[serde(default)]
    pub embedding_model: Option<String>,

    /// Claude model to use for `curate dedup` LLM calls.
    /// Defaults to `"haiku"` (latest Haiku via CLI alias).
    #[serde(default)]
    pub dedup_model: Option<String>,

    /// Per-project default Claude model. Falls below `prd_metadata.default_model`
    /// and above `user_config.default_model` in the resolution chain (see
    /// `loop_engine::model::resolve_task_model`).
    #[serde(default)]
    pub default_model: Option<String>,

    /// llama-box reranker endpoint. Must be set together with `reranker_model`;
    /// if only one is present the reranker is disabled with a warning.
    #[serde(default)]
    pub reranker_url: Option<String>,

    /// Cross-encoder model name served by the llama-box `/v1/rerank` endpoint.
    #[serde(default)]
    pub reranker_model: Option<String>,

    /// How many candidates per backend to fetch before reranking.
    /// Defaults to 3 when unset; values of 0 are clamped to 1 with a warning.
    #[serde(default)]
    pub reranker_over_fetch: Option<u32>,

    /// Hard cap (seconds) on a single parallel-slot merge-conflict resolution
    /// Claude run. Defaults to 600 (10 min). Lift for projects with large
    /// merges; lower for tight feedback loops.
    #[serde(default)]
    pub merge_resolver_timeout_secs: Option<u64>,

    /// `--effort` value passed to Claude when resolving a parallel-slot merge
    /// conflict. Defaults to `"medium"`. Use `"high"` for cross-cutting
    /// refactors that conflict on semantic logic.
    #[serde(default)]
    pub merge_resolver_effort: Option<String>,

    /// Halt the loop after this many *consecutive* parallel-slot merge-back
    /// failure waves. Default: `2` — a single failed merge is recoverable
    /// (next wave gets a clean slate from the resolver), but two in a row
    /// indicate a cascading state where letting more waves run risks the
    /// kind of branch divergence the mw-datalake incident produced.
    ///
    /// Threshold semantics:
    /// - `0` — never halt (legacy "log and continue" behavior preserved bit-for-bit)
    /// - `1` — halt on any merge-back failure
    /// - `2` (default) — halt after two consecutive merge-back failure waves
    #[serde(default = "default_merge_fail_halt_threshold")]
    pub merge_fail_halt_threshold: u32,

    /// Project-level extension to the baseline `IMPLICIT_OVERLAP_FILES` list
    /// used by `select_parallel_group` (FEAT-003). Match is by basename across
    /// any path in a task's `touchesFiles`. Extends rather than replaces the
    /// baseline so users opt IN to extra shared-infra files (e.g. an in-house
    /// `gradle-wrapper.lock`) without losing the language defaults.
    #[serde(default)]
    pub implicit_overlap_files: Vec<String>,

    /// Maximum number of stash-pop conflicts per slot per run before the slot
    /// is demoted to `failed_slots(PreResolver)` and the consecutive-merge-fail
    /// halt threshold trips. Controlled by the bounded warn-and-continue policy
    /// in `cleanup_preparation` (FEAT-003).
    ///
    /// Threshold semantics:
    /// - `0` — never halt on stash-pop conflicts (matches `merge_fail_halt_threshold == 0`)
    /// - `5` (default) — halt after 5 stash-pop conflict events on the same slot
    #[allow(dead_code)]
    #[serde(default = "default_slot_stash_limit")]
    pub slot_stash_limit: u32,

    /// Whether to auto-launch `/review-loop` after a successful loop/batch run.
    /// Default: `true`. Set to `false` to suppress the interactive review session.
    /// CLI flags `--auto-review` / `--no-auto-review` override this value.
    #[serde(default = "default_auto_review")]
    pub auto_review: bool,

    /// Minimum number of completed tasks required to trigger auto-review.
    /// Runs that completed fewer than this many tasks are not reviewed automatically.
    /// Default: `3`.
    #[serde(default = "default_auto_review_min_tasks")]
    pub auto_review_min_tasks: u32,

    /// Grok fallback runner configuration. Absent key → `None`; explicit
    /// `null` → `None`; explicit object → `Some` (with per-field defaults
    /// applied for any omitted optional fields). Default: `None`.
    #[serde(default)]
    pub fallback_runner: Option<FallbackRunnerConfig>,

    /// Optional model to route review-class tasks to (`CODE-REVIEW-*`,
    /// `MILESTONE-FINAL`, `REVIEW-*`). When set, these tasks are dispatched
    /// using this model instead of the default Claude model. Typically a Grok
    /// model id (e.g. `"grok-build"`). Absent key or explicit `null` → `None`.
    #[serde(default)]
    pub review_model: Option<String>,

    /// Primary runner routing configuration. Absent key → `None`; explicit
    /// `null` → `None`; explicit object → `Some` (with per-field defaults
    /// applied for any omitted optional fields). Default: `None`.
    ///
    /// LEGACY (FR-002 hard break): `default_model` / `review_model` /
    /// `fallback_runner` / `primary_runner` are no longer honored by model
    /// resolution. They still deserialize so the OLD resolution chain
    /// (`resolve_task_execution_target` & friends) compiles until REFACTOR-005
    /// removes both the chain and these four fields in one diff. A config that
    /// actually SETS any of these keys hard-errors at the loop/batch preflight
    /// and warns on non-loop reads — see [`detect_legacy_model_keys`] /
    /// [`preflight_validate_and_probe`] / [`read_project_config`].
    #[serde(default)]
    pub primary_runner: Option<PrimaryRunnerConfig>,

    /// Provider-first model config (FR-001): the SOLE model-routing surface
    /// going forward. NOT serde-derived — a sparse user override deep-merges
    /// onto [`ModelsConfig::builtin_default`] (see [`merge_models_config`]), so
    /// [`read_project_config`] populates this field explicitly. A config with no
    /// `models` key gets the built-in default (Claude enabled across the ladder).
    #[serde(skip)]
    pub models: ModelsConfig,

    /// Routing policy (FR-001): role-split / spillover layered over the anchor
    /// window. Like `models`, populated explicitly by [`read_project_config`]
    /// (NOT serde-derived) so absent → the empty default.
    #[serde(skip)]
    pub routing: RoutingConfig,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            version: 1,
            additional_allowed_tools: Vec::new(),
            permission_mode: None,
            ollama_url: None,
            embedding_model: None,
            dedup_model: None,
            default_model: None,
            reranker_url: None,
            reranker_model: None,
            reranker_over_fetch: None,
            merge_resolver_timeout_secs: None,
            merge_resolver_effort: None,
            merge_fail_halt_threshold: default_merge_fail_halt_threshold(),
            implicit_overlap_files: Vec::new(),
            slot_stash_limit: default_slot_stash_limit(),
            auto_review: default_auto_review(),
            auto_review_min_tasks: default_auto_review_min_tasks(),
            fallback_runner: None,
            review_model: None,
            primary_runner: None,
            models: ModelsConfig::builtin_default(),
            routing: RoutingConfig::default(),
        }
    }
}

impl ProjectConfig {
    /// Returns `Some((url, model, over_fetch))` only when both `reranker_url`
    /// AND `reranker_model` are set. Returns `None` silently when neither is
    /// set; warns and returns `None` when exactly one is present.
    pub fn resolved_reranker_config(&self) -> Option<(String, String, u32)> {
        match (&self.reranker_url, &self.reranker_model) {
            (Some(url), Some(model)) => {
                let over_fetch = match self.reranker_over_fetch {
                    None => 3,
                    Some(0) => {
                        crate::output::warn("rerankerOverFetch=0 is invalid; clamping to 1");
                        1
                    }
                    Some(n) => n,
                };
                Some((url.clone(), model.clone(), over_fetch))
            }
            (None, None) => None,
            _ => {
                crate::output::warn(
                    "rerankerUrl/rerankerModel: both must be set; reranker disabled",
                );
                None
            }
        }
    }
}

fn default_version() -> u32 {
    1
}

/// Default consecutive-merge-fail threshold (2). Single failures are recoverable;
/// two-in-a-row indicate a cascade.
fn default_merge_fail_halt_threshold() -> u32 {
    2
}

/// Default per-slot per-run stash-pop conflict limit (5).
fn default_slot_stash_limit() -> u32 {
    5
}

/// Auto-review is enabled by default.
fn default_auto_review() -> bool {
    true
}

/// Minimum completed tasks before auto-review fires (default 3).
fn default_auto_review_min_tasks() -> u32 {
    3
}

/// Default provider name for the Grok fallback runner (PRD §6).
fn default_fallback_provider() -> String {
    "grok".to_string()
}

/// Default model ID for the Grok fallback runner (PRD §6).
fn default_fallback_model() -> String {
    "grok-build".to_string()
}

/// Default consecutive-RuntimeError threshold before Grok fallback fires (PRD §3 US-005).
fn default_fallback_runtime_error_threshold() -> u32 {
    2
}

/// Default consecutive-RuntimeError threshold before primary-runner falls back to Claude.
fn default_primary_runtime_error_threshold() -> u32 {
    2
}

/// Check that `path` exists, is a regular file, and (on Unix) has the
/// executable bit set for some user class. Spawn will only succeed against
/// an executable file, so the startup probe should reject non-executable
/// paths up-front rather than letting them fail with a less-helpful
/// `std::io::Error` at first promotion. On non-Unix targets, falls back to
/// `exists()` (no mode bits available).
fn is_executable_path(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Resolve and probe the Grok binary path.
///
/// Resolution order (matches `runner::resolve_grok_binary`):
/// 1. `GROK_BINARY` env var when set AND non-empty/non-whitespace.
/// 2. `fallback_cli_binary` when set AND non-empty/non-whitespace — probed verbatim.
/// 3. Bare name `"grok"` — searches PATH directories.
///
/// Returns `Ok(())` when the resolved binary is an executable file.
/// Returns `Err(binary_name)` when it is missing or not executable.
///
/// Empty/whitespace values fall through to the next link — common shell
/// footgun (`export GROK_BINARY=""` must not cause a misleading failure
/// when grok is on PATH).
fn resolve_and_verify_grok_binary(fallback_cli_binary: Option<&str>) -> Result<(), String> {
    let env_bin = std::env::var("GROK_BINARY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let cli_bin = fallback_cli_binary
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string);

    let (binary, found) = if let Some(env_bin) = env_bin {
        let exec = is_executable_path(std::path::Path::new(&env_bin));
        (env_bin, exec)
    } else if let Some(explicit) = cli_bin {
        let exec = is_executable_path(std::path::Path::new(&explicit));
        (explicit, exec)
    } else {
        let name = "grok";
        let found = std::env::var_os("PATH")
            .map(|path_var| {
                std::env::split_paths(&path_var).any(|dir| is_executable_path(&dir.join(name)))
            })
            .unwrap_or(false);
        (name.to_string(), found)
    };

    if found { Ok(()) } else { Err(binary) }
}

fn resolve_and_verify_codex_binary() -> Result<(), String> {
    let env_bin = std::env::var("CODEX_BINARY")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let (binary, found) = if let Some(env_bin) = env_bin {
        let exec = is_executable_path(std::path::Path::new(&env_bin));
        (env_bin, exec)
    } else {
        let name = "codex";
        let found = std::env::var_os("PATH")
            .map(|path_var| {
                std::env::split_paths(&path_var).any(|dir| is_executable_path(&dir.join(name)))
            })
            .unwrap_or(false);
        (name.to_string(), found)
    };

    if found { Ok(()) } else { Err(binary) }
}

/// Verify that the Grok fallback binary is reachable at loop startup.
///
/// Returns `Ok(())` when `cfg` is `None` or `cfg.enabled` is `false`.
/// Returns `Err` when `cfg.enabled` is `true` and the binary is missing or
/// not executable. The error names the binary for operator diagnostics.
pub fn check_fallback_runner_binary(cfg: Option<&FallbackRunnerConfig>) -> TaskMgrResult<()> {
    let cfg = match cfg {
        None => return Ok(()),
        Some(c) if !c.enabled => return Ok(()),
        Some(c) => c,
    };
    resolve_and_verify_grok_binary(cfg.cli_binary.as_deref()).map_err(|binary| {
        TaskMgrError::NotFound {
            resource_type: "Fallback runner binary".to_string(),
            id: format!(
                "{binary} — install the Grok CLI or set `fallbackRunner.cliBinary` to the \
                 correct path (must be an executable file), then retry"
            ),
        }
    })
}

pub fn validate_runner_routing_config(cfg: &ProjectConfig) -> TaskMgrResult<()> {
    if let Some(fallback) = cfg.fallback_runner.as_ref()
        && fallback.enabled
        && !fallback.provider.trim().eq_ignore_ascii_case("grok")
    {
        return Err(TaskMgrError::InvalidConfig {
            field: "fallbackRunner.provider".to_string(),
            message: "v1 fallbackRunner only supports provider \"grok\"".to_string(),
        });
    }
    if let Some(primary) = cfg.primary_runner.as_ref() {
        for (map_name, key, spec) in primary_runner_specs(primary) {
            if spec.provider.trim().is_empty() {
                return Err(TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.provider"),
                    message: "provider must not be blank".to_string(),
                });
            }
            // Strict parse: surface the error from `parse_config_provider`
            // (typos like "openai" / "codex-cli" / "groq") so a misspelled
            // provider hard-fails at validation instead of silently routing
            // the task to Claude. Returning `Option::None` from a parser is
            // the exact silent-fallback footgun this branch defends against.
            let provider = parse_config_provider(&spec.provider).map_err(|message| {
                TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.provider"),
                    message,
                }
            })?;
            if spec.model.trim().is_empty() && provider != Provider::Codex {
                return Err(TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.model"),
                    message: "model must not be blank unless provider is codex".to_string(),
                });
            }
            if spec.model.trim().starts_with('-') {
                return Err(TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.model"),
                    message: format!(
                        "model must not start with `-` (got {:?}); a leading `-` would be \
                         interpreted as a CLI flag by the child runner",
                        spec.model.trim(),
                    ),
                });
            }
        }
        for (prefix, tier_map) in &primary.baseline_tier_routes {
            for tier_key in tier_map.keys() {
                parse_baseline_tier_key(tier_key).map_err(|message| {
                    TaskMgrError::InvalidConfig {
                        field: format!("primaryRunner.baselineTierRoutes.{prefix}.{tier_key}"),
                        message,
                    }
                })?;
            }
        }
        validate_non_conflicting_prefix_routes(primary)?;
        if cfg
            .review_model
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && primary_runner_contains_codex_review_route(primary)
        {
            return Err(TaskMgrError::InvalidConfig {
                field: "reviewModel".to_string(),
                message: "reviewModel is string-only in v1 and overrides primaryRunner Codex review routes; unset reviewModel or remove the Codex review route".to_string(),
            });
        }
    }
    Ok(())
}

fn validate_non_conflicting_prefix_routes(primary: &PrimaryRunnerConfig) -> TaskMgrResult<()> {
    validate_by_id_prefix_conflicts(primary)?;
    validate_baseline_tier_prefix_conflicts(primary)?;
    Ok(())
}

fn validate_by_id_prefix_conflicts(primary: &PrimaryRunnerConfig) -> TaskMgrResult<()> {
    let mut routes: Vec<_> = primary.by_id_prefix.iter().collect();
    routes.sort_by_key(|(left_key, _)| *left_key);

    for (i, (left_key, left_spec)) in routes.iter().enumerate() {
        for (right_key, right_spec) in routes.iter().skip(i + 1) {
            if left_spec == right_spec || !route_prefixes_overlap(left_key, right_key) {
                continue;
            }
            return Err(TaskMgrError::InvalidConfig {
                field: "primaryRunner.byIdPrefix".to_string(),
                message: format!(
                    "conflicting overlapping prefixes {left_key:?} and {right_key:?}; \
                     {left:?} and {right:?} can match the same task id but route to \
                     different specs",
                    left = normalize_route_prefix(left_key),
                    right = normalize_route_prefix(right_key),
                ),
            });
        }
    }
    Ok(())
}

fn validate_baseline_tier_prefix_conflicts(primary: &PrimaryRunnerConfig) -> TaskMgrResult<()> {
    let mut routes = Vec::new();
    for (prefix, tier_map) in &primary.baseline_tier_routes {
        for (tier_key, spec) in tier_map {
            let tier = parse_baseline_tier_key(tier_key).map_err(|message| {
                TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.baselineTierRoutes.{prefix}.{tier_key}"),
                    message,
                }
            })?;
            routes.push((prefix, tier_key, tier, spec));
        }
    }
    routes.sort_by(
        |(left_prefix, left_tier_key, _, _), (right_prefix, right_tier_key, _, _)| {
            left_prefix
                .cmp(right_prefix)
                .then_with(|| left_tier_key.cmp(right_tier_key))
        },
    );

    for (i, (left_prefix, left_tier_key, left_tier, left_spec)) in routes.iter().enumerate() {
        for (right_prefix, right_tier_key, right_tier, right_spec) in routes.iter().skip(i + 1) {
            if left_tier != right_tier
                || left_spec == right_spec
                || !route_prefixes_overlap(left_prefix, right_prefix)
            {
                continue;
            }
            return Err(TaskMgrError::InvalidConfig {
                field: "primaryRunner.baselineTierRoutes".to_string(),
                message: format!(
                    "conflicting overlapping prefixes {left_prefix:?}.{left_tier_key} and \
                     {right_prefix:?}.{right_tier_key}; both normalize to tier {left_tier:?} \
                     and can match the same task id but route to different specs",
                ),
            });
        }
    }
    Ok(())
}

pub fn check_codex_runner_binary(primary: Option<&PrimaryRunnerConfig>) -> TaskMgrResult<()> {
    let Some(primary) = primary else {
        return Ok(());
    };
    if !primary_runner_specs(primary)
        .any(|(_, _, spec)| parse_config_provider(&spec.provider).ok() == Some(Provider::Codex))
    {
        return Ok(());
    }
    resolve_and_verify_codex_binary().map_err(|binary| TaskMgrError::NotFound {
        resource_type: "Codex CLI binary required by primaryRunner".to_string(),
        id: format!(
            "{binary} — install Codex CLI or set CODEX_BINARY to an executable file, then retry"
        ),
    })
}

fn primary_runner_specs(
    primary: &PrimaryRunnerConfig,
) -> impl Iterator<Item = (&'static str, String, &RunnerSpec)> {
    primary
        .by_task_type
        .iter()
        .map(|(k, v)| ("byTaskType", k.clone(), v))
        .chain(
            primary
                .by_id_prefix
                .iter()
                .map(|(k, v)| ("byIdPrefix", k.clone(), v)),
        )
        .chain(
            primary
                .baseline_tier_routes
                .iter()
                .flat_map(|(prefix, tiers)| {
                    tiers.iter().map(move |(tier, spec)| {
                        ("baselineTierRoutes", format!("{prefix}.{tier}"), spec)
                    })
                }),
        )
}

fn primary_runner_contains_codex_review_route(primary: &PrimaryRunnerConfig) -> bool {
    primary_runner_specs(primary).any(|(map_name, key, spec)| {
        parse_config_provider(&spec.provider).ok() == Some(Provider::Codex)
            && match map_name {
                "byTaskType" => {
                    key.eq_ignore_ascii_case("review")
                        || key.eq_ignore_ascii_case("code-review")
                        || key.eq_ignore_ascii_case("milestone-final")
                }
                "byIdPrefix" => {
                    let k = key.trim_end_matches('-').to_ascii_uppercase();
                    matches!(k.as_str(), "CODE-REVIEW" | "REVIEW" | "MILESTONE-FINAL")
                }
                "baselineTierRoutes" => {
                    let prefix = key
                        .split_once('.')
                        .map(|(prefix, _)| prefix)
                        .unwrap_or(key.as_str());
                    let k = prefix.trim_end_matches('-').to_ascii_uppercase();
                    matches!(k.as_str(), "CODE-REVIEW" | "REVIEW" | "MILESTONE-FINAL")
                }
                _ => false,
            }
    })
}

/// Verify that the Grok binary is reachable when `reviewModel` routes to Grok.
///
/// Returns `Ok(())` when `review_model` is `None` or resolves to a non-Grok
/// provider. Returns `Err` when it resolves to Grok and the binary is missing
/// or not executable. The error names the model and binary for diagnostics.
pub fn check_review_model_binary(
    review_model: Option<&str>,
    fallback_cli_binary: Option<&str>,
) -> TaskMgrResult<()> {
    use crate::loop_engine::model::{Provider, provider_for_model};

    if provider_for_model(review_model) != Provider::Grok {
        return Ok(());
    }
    resolve_and_verify_grok_binary(fallback_cli_binary).map_err(|binary| TaskMgrError::NotFound {
        resource_type: "Grok CLI binary required by reviewModel".to_string(),
        id: format!(
            "{binary} — install the Grok CLI or set `fallbackRunner.cliBinary` to the \
                 correct path, then run `grok login` to authenticate \
                 (reviewModel = {rm})",
            rm = review_model.unwrap_or("<unknown>")
        ),
    })
}

/// Startup pre-flight (FR-002 hard break): reject any legacy model-config keys,
/// then validate the provider-first `models`/`routing` block and probe every
/// ENABLED provider's CLI binary, BEFORE the first iteration.
///
/// This is the single source of truth for "is this project safe to run?" and
/// MUST be called from every loop entry point — both `loop run` (single PRD)
/// and `batch run` (N PRDs). Hoisting it here closes the parity gap where a
/// misconfigured config would surface only on `loop run`, but run unvalidated
/// under `batch run`.
///
/// Order (load-bearing):
/// 1. **Legacy-key hard error.** A config that still carries any of
///    `defaultModel`/`reviewModel`/`primaryRunner`/`fallbackRunner` is rejected
///    up front, naming each present key + printing the FR-001 skeleton +
///    pointing at `models init --force-replace-legacy`. Reads the raw JSON
///    (not the parsed struct) so it can name the exact keys present.
/// 2. **User-config `defaultModel` deprecation warning** (non-fatal): emitted at
///    the loop/batch preflight ONLY, never on every non-loop read.
/// 3. **Pure validation** (`validate_models_config`): every config error in one
///    pass — no I/O. An operator who mis-typed a provider/tier sees the
///    structured config error, not a misleading "binary missing" message.
/// 4. **Enabled-only binary probes** (`probe_enabled_provider_binaries`):
///    separate from validation; a pure-Claude config triggers no `grok`/`codex`
///    PATH lookup because disabled providers are skipped.
///
/// Failure semantics for `batch run`: a failure here aborts the WHOLE batch
/// before any PRD runs. Config validity and binary availability are
/// project-level (every PRD in the batch shares the same `.task-mgr/config.json`
/// and `$PATH`), so a failure affects every PRD equally — failing fast up-front
/// mirrors `loop run`'s fail-before-iteration-1 contract and avoids burning N
/// partial runs on a uniformly-broken environment.
pub fn preflight_validate_and_probe(db_dir: &Path, cfg: &ProjectConfig) -> TaskMgrResult<()> {
    // 1. Hard break: legacy keys are fatal at the loop/batch entry.
    reject_legacy_model_config(db_dir)?;

    // 2. Deprecated user-level defaultModel: warn (not fatal) here only.
    if crate::loop_engine::user_config::read_user_config()
        .default_model
        .is_some()
    {
        crate::output::warn(
            "user config `defaultModel` is ignored under the models config; use \
             models.anchor / routing instead",
        );
    }

    // 3. Pure validation, then 4. enabled-only probes — distinct steps.
    validate_models_config(&cfg.models, &cfg.routing).map_err(|errors| {
        TaskMgrError::InvalidConfig {
            field: "models".to_string(),
            message: errors.join("; "),
        }
    })?;
    let resolved = crate::loop_engine::model::resolve_models_config(&cfg.models, &cfg.routing);
    probe_enabled_provider_binaries(&resolved)?;
    Ok(())
}

/// Hard-error when `<db_dir>/config.json` still carries any legacy model-config
/// key. Reads the RAW JSON so the error can name each present key exactly.
/// A missing or malformed config file is not this function's concern — it is
/// handled (warned, not fatal) by [`read_project_config`].
fn reject_legacy_model_config(db_dir: &Path) -> TaskMgrResult<()> {
    let path = db_dir.join("config.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Ok(());
    };
    let legacy = detect_legacy_model_keys(&value);
    if legacy.is_empty() {
        return Ok(());
    }
    Err(TaskMgrError::InvalidConfig {
        field: "models".to_string(),
        message: legacy_model_keys_message(&legacy),
    })
}

/// Read project config from `<db_dir>/config.json`.
///
/// Returns the default config if the file doesn't exist. Warns (never fails) on
/// invalid JSON, returning defaults instead — non-loop commands (`recall`,
/// `curate`, `next`, `models show`, …) must keep working on a broken config.
///
/// FR-002 hard break: legacy model-config keys are no longer honored. When any
/// are present this emits ONE stderr warning and proceeds with DEFAULT model
/// routing — the loop/batch preflight is the only place those keys hard-error
/// (see [`preflight_validate_and_probe`]). The provider-first `models`/`routing`
/// surfaces are populated explicitly (a sparse `models` override deep-merges
/// onto the built-in default; plain serde would replace the whole ladder).
pub fn read_project_config(db_dir: &Path) -> ProjectConfig {
    let path = db_dir.join("config.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return ProjectConfig::default();
    };
    let value: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(value) => value,
        Err(e) => {
            crate::output::warn(&format!("Invalid .task-mgr/config.json: {e}"));
            return ProjectConfig::default();
        }
    };
    // Non-loop read path: legacy keys warn ONCE and proceed (never fatal).
    let legacy = detect_legacy_model_keys(&value);
    if !legacy.is_empty() {
        crate::output::warn(&format!(
            ".task-mgr/config.json carries legacy model key(s) [{}] which are ignored under the \
             provider-first `models`/`routing` config; run `task-mgr models init \
             --force-replace-legacy` to migrate.",
            legacy.join(", ")
        ));
    }
    // Deserialize the non-model surfaces. The legacy fields still parse (they
    // remain on the struct until REFACTOR-005) but never feed resolution; the
    // `models`/`routing` fields are `#[serde(skip)]` and set below.
    let mut cfg: ProjectConfig = match serde_json::from_value(value.clone()) {
        Ok(cfg) => cfg,
        Err(e) => {
            crate::output::warn(&format!("Invalid .task-mgr/config.json: {e}"));
            return ProjectConfig::default();
        }
    };
    cfg.models = match merge_models_config(value.get("models")) {
        Ok(models) => models,
        Err(e) => {
            crate::output::warn(&format!("Invalid `models` config: {e}; using defaults"));
            ModelsConfig::builtin_default()
        }
    };
    cfg.routing = match value.get("routing") {
        Some(routing) => serde_json::from_value(routing.clone()).unwrap_or_else(|e| {
            crate::output::warn(&format!("Invalid `routing` config: {e}; using defaults"));
            RoutingConfig::default()
        }),
        None => RoutingConfig::default(),
    };
    cfg
}

/// Set (or clear) the `defaultModel` field in `<db_dir>/config.json` without
/// clobbering other fields — including unknown forward-compat ones.
///
/// Pass `Some(model)` to set, `None` to remove the key entirely.
/// Creates the file and parent dir if needed. Writes atomically via a
/// same-directory tempfile + rename so readers never see a half-written JSON.
/// Tolerant: malformed JSON is silently replaced by the `{"version":1}` seed.
pub fn write_default_model(db_dir: &Path, model: Option<&str>) -> std::io::Result<()> {
    let path = db_dir.join("config.json");
    write_config_key_at(
        &path,
        "defaultModel",
        model.map(|m| serde_json::Value::String(m.to_string())),
        serde_json::json!({ "version": 1 }),
        OnCorruptJson::UseSeed,
    )
}

/// Set (or clear) the `reviewModel` field in `<db_dir>/config.json` without
/// clobbering other fields.
///
/// Pass `Some(model)` to set, `None` to remove the key.
/// Creates the file if absent (`{"version":1}` + the target key).
/// Returns `Err` if the existing file contains malformed JSON (path named in message).
pub fn write_review_model(db_dir: &Path, model: Option<&str>) -> std::io::Result<()> {
    let path = db_dir.join("config.json");
    write_config_key_at(
        &path,
        "reviewModel",
        model.map(|m| serde_json::Value::String(m.to_string())),
        serde_json::json!({ "version": 1 }),
        OnCorruptJson::ReturnError,
    )
}

/// Set (or clear) the `fallbackRunner` block in `<db_dir>/config.json` without
/// clobbering other fields.
///
/// Pass `Some(cfg)` to set, `None` to remove the key.
/// Creates the file if absent (`{"version":1}` + the target key).
/// Returns `Err` if the existing file contains malformed JSON (path named in message).
pub fn write_fallback_runner(
    db_dir: &Path,
    cfg: Option<&FallbackRunnerConfig>,
) -> std::io::Result<()> {
    let path = db_dir.join("config.json");
    let v = cfg
        .map(|c| {
            serde_json::to_value(c)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })
        .transpose()?;
    write_config_key_at(
        &path,
        "fallbackRunner",
        v,
        serde_json::json!({ "version": 1 }),
        OnCorruptJson::ReturnError,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::test_utils::{CLAUDE_BINARY_MUTEX, EnvGuard};
    use std::fs;

    /// Serializes tests that mutate the process-global `CODEX_BINARY` env var
    /// and then probe it (directly or via `preflight_validate_and_probe`).
    ///
    /// Under `cargo test`'s default multi-threaded runner these tests race on
    /// the shared env var and against the PATH-reading binary probe: a sibling
    /// test removing/restoring `CODEX_BINARY` mid-flight can make the probe fall
    /// through to a real `codex` on PATH and flip an `expect_err` to a pass.
    /// A module-local `Mutex` is the minimal, dependency-free serializer
    /// (no `serial_test` crate). `GROK_BINARY`-mutating tests in this module
    /// use the cross-file [`crate::loop_engine::test_utils::GROK_BINARY_MUTEX`]
    /// instead, because `GROK_BINARY` is also mutated by tests in other lib
    /// modules (`runner.rs`, `commands::models::handlers`) that share this test
    /// binary; a module-local lock would not serialize against those.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_read_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert!(config.additional_allowed_tools.is_empty());
    }

    #[test]
    fn test_read_invalid_json_warns_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "not json").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert!(config.additional_allowed_tools.is_empty());
    }

    #[test]
    fn test_read_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "additionalAllowedTools": ["Bash(docker:*)", "Bash(curl:*)"]}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert_eq!(
            config.additional_allowed_tools,
            vec!["Bash(docker:*)", "Bash(curl:*)"]
        );
    }

    #[test]
    fn test_read_config_with_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "additionalAllowedTools": ["Bash(docker:*)"], "futureField": true}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.additional_allowed_tools, vec!["Bash(docker:*)"]);
    }

    #[test]
    fn test_default_version() {
        let config = ProjectConfig::default();
        assert_eq!(config.version, 1);
    }

    #[test]
    fn test_empty_json_object_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert!(config.additional_allowed_tools.is_empty());
        assert!(config.permission_mode.is_none());
    }

    #[test]
    fn test_permission_mode_dangerous() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"permissionMode": "dangerous"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.permission_mode.as_deref(), Some("dangerous"));
    }

    #[test]
    fn test_permission_mode_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"additionalAllowedTools": []}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.permission_mode.is_none());
    }

    #[test]
    fn test_ollama_url_and_embedding_model() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"ollamaUrl": "http://gpu-server:11434", "embeddingModel": "custom-model"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(
            config.ollama_url.as_deref(),
            Some("http://gpu-server:11434")
        );
        assert_eq!(config.embedding_model.as_deref(), Some("custom-model"));
    }

    #[test]
    fn test_ollama_url_and_embedding_model_default_to_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.ollama_url.is_none());
        assert!(config.embedding_model.is_none());
    }

    #[test]
    fn test_default_model_reads() {
        use crate::loop_engine::model::SONNET_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(r#"{{"defaultModel": "{SONNET_MODEL}"}}"#),
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.default_model.as_deref(), Some(SONNET_MODEL));
    }

    #[test]
    fn test_write_default_model_creates_file() {
        use crate::loop_engine::model::OPUS_MODEL;
        let dir = tempfile::tempdir().unwrap();
        write_default_model(dir.path(), Some(OPUS_MODEL)).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.default_model.as_deref(), Some(OPUS_MODEL));
    }

    #[test]
    fn test_write_default_model_preserves_unknown_fields() {
        use crate::loop_engine::model::HAIKU_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "futureField": {"nested": true}, "additionalAllowedTools": ["Bash(docker:*)"]}"#,
        )
        .unwrap();
        write_default_model(dir.path(), Some(HAIKU_MODEL)).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("futureField"),
            "unknown field must survive the write"
        );
        assert!(raw.contains("nested"));
        assert!(raw.contains("Bash(docker:*)"));
        assert!(raw.contains(HAIKU_MODEL));
    }

    #[test]
    fn test_write_default_model_removes_key_when_none() {
        use crate::loop_engine::model::OPUS_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(r#"{{"defaultModel": "{OPUS_MODEL}", "version": 1}}"#),
        )
        .unwrap();
        write_default_model(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("defaultModel"),
            "key should be removed, got: {raw}"
        );
    }

    #[test]
    fn test_resolved_reranker_config_both_set() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: Some("m".to_string()),
            reranker_over_fetch: Some(5),
            ..Default::default()
        };
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 5))
        );
    }

    #[test]
    fn test_resolved_reranker_config_default_over_fetch() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: Some("m".to_string()),
            reranker_over_fetch: None,
            ..Default::default()
        };
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 3))
        );
    }

    #[test]
    fn test_resolved_reranker_config_over_fetch_zero_clamped_to_one() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: Some("m".to_string()),
            reranker_over_fetch: Some(0),
            ..Default::default()
        };
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 1))
        );
    }

    #[test]
    fn test_resolved_reranker_config_only_url_set() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: None,
            ..Default::default()
        };
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_resolved_reranker_config_only_model_set() {
        let config = ProjectConfig {
            reranker_url: None,
            reranker_model: Some("m".to_string()),
            ..Default::default()
        };
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_resolved_reranker_config_neither_set() {
        let config = ProjectConfig::default();
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_resolved_reranker_config_from_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_reranker_config_deserializes_from_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"rerankerUrl":"http://x","rerankerModel":"m","rerankerOverFetch":5}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.reranker_url.as_deref(), Some("http://x"));
        assert_eq!(config.reranker_model.as_deref(), Some("m"));
        assert_eq!(config.reranker_over_fetch, Some(5));
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 5))
        );
    }

    #[test]
    fn test_merge_fail_halt_threshold_default_is_two() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 2);
    }

    #[test]
    fn test_merge_fail_halt_threshold_default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 2);
    }

    #[test]
    fn test_merge_fail_halt_threshold_can_be_zero() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"mergeFailHaltThreshold": 0}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 0);
    }

    #[test]
    fn test_merge_fail_halt_threshold_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"mergeFailHaltThreshold": 5}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 5);
    }

    #[test]
    fn test_default_struct_has_threshold_two() {
        let config = ProjectConfig::default();
        assert_eq!(config.merge_fail_halt_threshold, 2);
    }

    #[test]
    fn test_implicit_overlap_files_default_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.implicit_overlap_files.is_empty());
    }

    #[test]
    fn test_implicit_overlap_files_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"implicitOverlapFiles": ["custom.lock", "gradle-wrapper.lock"]}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(
            config.implicit_overlap_files,
            vec!["custom.lock".to_string(), "gradle-wrapper.lock".to_string()]
        );
    }

    #[test]
    fn test_implicit_overlap_files_default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.implicit_overlap_files.is_empty());
    }

    #[test]
    fn test_write_default_model_recovers_from_corrupt_json() {
        use crate::loop_engine::model::SONNET_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "not json at all").unwrap();
        // Write recovers by starting fresh — doesn't lose data that wasn't parseable anyway.
        write_default_model(dir.path(), Some(SONNET_MODEL)).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.default_model.as_deref(), Some(SONNET_MODEL));
    }

    #[test]
    fn test_slot_stash_limit_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"slotStashLimit":10}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.slot_stash_limit, 10);
    }

    #[test]
    fn test_slot_stash_limit_default_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.slot_stash_limit, 5);
    }

    #[test]
    fn test_slot_stash_limit_accepts_zero() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"slotStashLimit":0}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.slot_stash_limit, 0);
    }

    #[test]
    fn test_slot_stash_limit_default_struct() {
        let config = ProjectConfig::default();
        assert_eq!(config.slot_stash_limit, 5);
    }

    #[test]
    fn test_auto_review_default_is_true() {
        // Default impl
        assert!(ProjectConfig::default().auto_review);

        // Missing file → defaults
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review);

        // Empty JSON → serde default fn fires (not bool's Default::default())
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review);
    }

    #[test]
    fn test_auto_review_min_tasks_default_is_three() {
        // Default impl
        assert_eq!(ProjectConfig::default().auto_review_min_tasks, 3);

        // Missing file → defaults
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.auto_review_min_tasks, 3);

        // Empty JSON → serde default fn fires
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.auto_review_min_tasks, 3);
    }

    #[test]
    fn test_review_model_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.review_model.is_none());
    }

    #[test]
    fn test_review_model_default_impl_is_none() {
        assert!(ProjectConfig::default().review_model.is_none());
    }

    #[test]
    fn test_review_model_deserializes_from_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"reviewModel": "grok-build"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.review_model.as_deref(), Some("grok-build"));
    }

    #[test]
    fn test_review_model_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.review_model.is_none());
    }

    #[test]
    fn test_review_model_snake_case_not_accepted() {
        // Wire name is camelCase; snake_case must not set the field.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"review_model": "grok-build"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(
            config.review_model.is_none(),
            "snake_case key must not set review_model"
        );
    }

    #[test]
    fn test_auto_review_round_trips_from_json() {
        let dir = tempfile::tempdir().unwrap();

        // Explicit false + explicit min_tasks
        fs::write(
            dir.path().join("config.json"),
            r#"{"autoReview": false, "autoReviewMinTasks": 0}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(!config.auto_review);
        assert_eq!(config.auto_review_min_tasks, 0);

        // Only autoReview=false, min_tasks stays at default
        fs::write(dir.path().join("config.json"), r#"{"autoReview": false}"#).unwrap();
        let config = read_project_config(dir.path());
        assert!(!config.auto_review);
        assert_eq!(config.auto_review_min_tasks, 3);

        // Only autoReviewMinTasks=5, auto_review stays at default true
        fs::write(
            dir.path().join("config.json"),
            r#"{"autoReviewMinTasks": 5}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review);
        assert_eq!(config.auto_review_min_tasks, 5);

        // snake_case keys are rejected — field stays at default true
        fs::write(dir.path().join("config.json"), r#"{"auto_review": false}"#).unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review, "snake_case key must not set the field");
    }

    // ---- check_review_model_binary tests ----

    #[test]
    fn test_check_review_model_binary_claude_provider_is_noop() {
        use crate::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};
        // When reviewModel resolves to Claude, the probe must succeed regardless
        // of whether grok is on PATH — no PATH lookup must occur.
        assert!(check_review_model_binary(Some(OPUS_MODEL), None).is_ok());
        assert!(check_review_model_binary(Some(SONNET_MODEL), None).is_ok());
    }

    #[test]
    fn test_check_review_model_binary_none_is_noop() {
        // Unset reviewModel → probe is always a no-op.
        assert!(check_review_model_binary(None, None).is_ok());
    }

    #[test]
    fn test_check_review_model_binary_grok_missing_binary_errors() {
        // reviewModel resolves to Grok AND the binary is absent → Err.
        // Inject a path that definitely doesn't exist as GROK_BINARY so the
        // probe never falls through to the system PATH. The mutex serializes
        // against other tests that mutate the GROK_BINARY env var.
        use crate::loop_engine::test_utils::GROK_BINARY_MUTEX;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bogus = "/tmp/task-mgr-test-nonexistent-grok-binary-xyz";
        unsafe { std::env::set_var("GROK_BINARY", bogus) };
        let result = check_review_model_binary(Some("grok-build"), None);
        unsafe { std::env::remove_var("GROK_BINARY") };
        assert!(result.is_err(), "missing grok binary must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("grok-build"),
            "error should mention the reviewModel value; got: {msg}"
        );
    }

    #[test]
    fn test_check_review_model_binary_grok_explicit_missing_cli_binary_errors() {
        // When fallbackRunner.cliBinary is set to a non-existent path AND
        // GROK_BINARY is unset, the probe checks the explicit path and errors.
        use crate::loop_engine::test_utils::GROK_BINARY_MUTEX;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bogus_cli = "/tmp/task-mgr-test-nonexistent-grok-cli-xyz";
        // Ensure GROK_BINARY is absent so we fall through to fallback_cli_binary.
        unsafe { std::env::remove_var("GROK_BINARY") };
        let result = check_review_model_binary(Some("grok-build"), Some(bogus_cli));
        assert!(
            result.is_err(),
            "non-existent cliBinary path must return Err"
        );
    }

    #[test]
    fn test_check_review_model_binary_groq_not_grok_is_noop() {
        // "groq-llama-3" must NOT be classified as Grok (token-equality rule).
        // Probe must succeed even if grok binary is absent.
        assert!(check_review_model_binary(Some("groq-llama-3"), None).is_ok());
    }

    // ---- PrimaryRunnerConfig serde round-trip tests ----

    #[test]
    fn test_primary_runner_absent_is_none() {
        // Missing `primaryRunner` key → `None`.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
    }

    #[test]
    fn test_primary_runner_explicit_null_is_none() {
        // Explicit JSON `null` → `None` (matches FallbackRunnerConfig behavior).
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"primaryRunner": null}"#).unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
    }

    #[test]
    fn test_primary_runner_present_empty_maps() {
        // Object present but no map entries → `Some` with empty maps and defaults.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"primaryRunner": {}}"#).unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert!(pr.claude_fallback_model.is_none());
        assert_eq!(pr.runtime_error_threshold, 2);
        assert!(pr.by_task_type.is_empty());
        assert!(pr.by_id_prefix.is_empty());
        assert!(pr.baseline_tier_routes.is_empty());
    }

    #[test]
    fn test_primary_runner_present_populated() {
        use crate::loop_engine::model::SONNET_MODEL;
        // Fully populated primaryRunner round-trips correctly.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(
                r#"{{
                "primaryRunner": {{
                    "claudeFallbackModel": "{SONNET_MODEL}",
                    "runtimeErrorThreshold": 3,
                    "byTaskType": {{
                        "review":    {{ "provider": "grok", "model": "grok-build" }},
                        "milestone": {{ "provider": "grok", "model": "grok-build" }}
                    }},
                    "byIdPrefix": {{
                        "REVIEW-":    {{ "provider": "grok", "model": "grok-build" }},
                        "MILESTONE-": {{ "provider": "grok", "model": "grok-build" }}
                    }},
                    "baselineTierRoutes": {{
                        "FEAT": {{
                            "high": {{ "provider": "codex", "runtimeErrorFallback": true }},
                            "standard": {{ "provider": "grok", "model": "grok-build" }}
                        }}
                    }}
                }}
            }}"#
            ),
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert_eq!(pr.claude_fallback_model.as_deref(), Some(SONNET_MODEL));
        assert_eq!(pr.runtime_error_threshold, 3);

        let review_spec = pr.by_task_type.get("review").expect("review key missing");
        assert_eq!(review_spec.provider, "grok");
        assert_eq!(review_spec.model, "grok-build");

        let milestone_spec = pr
            .by_task_type
            .get("milestone")
            .expect("milestone key missing");
        assert_eq!(milestone_spec.provider, "grok");
        assert_eq!(milestone_spec.model, "grok-build");

        let rev_prefix_spec = pr.by_id_prefix.get("REVIEW-").expect("REVIEW- key missing");
        assert_eq!(rev_prefix_spec.provider, "grok");
        assert_eq!(rev_prefix_spec.model, "grok-build");

        let ms_prefix_spec = pr
            .by_id_prefix
            .get("MILESTONE-")
            .expect("MILESTONE- key missing");
        assert_eq!(ms_prefix_spec.provider, "grok");
        assert_eq!(ms_prefix_spec.model, "grok-build");

        let feat_tiers = pr
            .baseline_tier_routes
            .get("FEAT")
            .expect("FEAT key missing");
        let high_spec = feat_tiers.get("high").expect("high key missing");
        assert_eq!(high_spec.provider, "codex");
        assert!(high_spec.runtime_error_fallback);
        let standard_spec = feat_tiers.get("standard").expect("standard key missing");
        assert_eq!(standard_spec.provider, "grok");
        assert_eq!(standard_spec.model, "grok-build");
    }

    #[test]
    fn test_primary_runner_partial_uses_defaults() {
        // Partial object: only byTaskType set → claudeFallbackModel is None,
        // runtimeErrorThreshold is 2, byIdPrefix is empty.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byTaskType": {
                        "review": { "provider": "grok", "model": "grok-build" }
                    }
                }
            }"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert!(
            pr.claude_fallback_model.is_none(),
            "claudeFallbackModel absent → None"
        );
        assert_eq!(pr.runtime_error_threshold, 2, "default threshold is 2");
        assert!(pr.by_id_prefix.is_empty(), "byIdPrefix absent → empty map");
        assert!(
            pr.baseline_tier_routes.is_empty(),
            "baselineTierRoutes absent → empty map"
        );
        let spec = pr.by_task_type.get("review").expect("review key missing");
        assert_eq!(spec.model, "grok-build");
    }

    #[test]
    fn test_primary_runner_absent_does_not_affect_existing_config() {
        // Ensures that adding `primary_runner` to ProjectConfig doesn't break
        // existing round-trips for other fields when primaryRunner is absent.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "additionalAllowedTools": ["Bash(docker:*)"], "mergeFailHaltThreshold": 3}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
        assert_eq!(config.version, 1);
        assert_eq!(config.additional_allowed_tools, vec!["Bash(docker:*)"]);
        assert_eq!(config.merge_fail_halt_threshold, 3);
    }

    #[test]
    fn test_primary_runner_default_impl_is_none() {
        assert!(ProjectConfig::default().primary_runner.is_none());
    }

    // ---- RunnerSpec.runtime_error_fallback serde tests (FEAT-005) ----

    #[test]
    fn test_runner_spec_fallback_to_claude_absent_defaults_to_false() {
        // AC: runtimeErrorFallback defaults to false; an absent field deserializes
        // to false. Existing Codex projects keep the legacy auto-block path.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byTaskType": {
                        "spike": { "provider": "codex" }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_task_type
            .remove("spike")
            .expect("spike key present");
        assert!(
            !spec.runtime_error_fallback,
            "absent runtimeErrorFallback must deserialize to false"
        );
    }

    #[test]
    fn test_runner_spec_runtime_error_fallback_true_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byIdPrefix": {
                        "SPIKE-": { "provider": "codex", "runtimeErrorFallback": true }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_id_prefix
            .remove("SPIKE-")
            .expect("SPIKE- key present");
        assert!(
            spec.runtime_error_fallback,
            "runtimeErrorFallback=true must round-trip"
        );
    }

    #[test]
    fn test_runner_spec_runtime_error_fallback_snake_case_rejected() {
        // CONTRACT: the field name on the wire is camelCase (runtimeErrorFallback),
        // matching the rest of RunnerSpec's serde rename_all. snake_case must
        // NOT silently set the field.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byTaskType": {
                        "spike": { "provider": "codex", "fallback_to_claude": true }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_task_type
            .remove("spike")
            .expect("spike key present");
        assert!(
            !spec.runtime_error_fallback,
            "snake_case key must not set runtime_error_fallback"
        );
    }

    // ---- preflight_validate_and_probe tests (FR-002 hard break) ----

    /// Create an executable stub a binary probe will accept via `cliBinary`.
    fn make_executable_stub(dir: &Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[test]
    fn test_preflight_hard_errors_on_legacy_keys_naming_each() {
        // FR-002 / edgeCases[3]: the loop/batch entry must HARD-ERROR on a
        // legacy-key config, naming every present key and pointing at the
        // migration command. This is the loop-side half of the warn-vs-error
        // split (read_project_config is the warn-and-proceed half).
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            serde_json::json!({
                "defaultModel": OPUS_MODEL,
                "reviewModel": SONNET_MODEL,
                "primaryRunner": {},
                "fallbackRunner": {}
            })
            .to_string(),
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let err = preflight_validate_and_probe(dir.path(), &cfg)
            .expect_err("legacy keys must hard-error at loop/batch preflight");
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
            "error must point at the migration command: {msg}"
        );
    }

    #[test]
    fn test_preflight_passes_clean_config_after_validation_and_probe() {
        // A legacy-free config validates and probes the enabled provider's
        // binary. Claude is enabled by default; point its cliBinary at a stub
        // so the probe resolves deterministically. Hold the CLAUDE_BINARY mutex
        // + clear the env var so a sibling test can't override the cliBinary.
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _claude_env = EnvGuard::remove("CLAUDE_BINARY");
        let dir = tempfile::tempdir().unwrap();
        let stub = make_executable_stub(dir.path(), "claude-stub");
        fs::write(
            dir.path().join("config.json"),
            serde_json::json!({
                "models": { "providers": { "claude": { "cliBinary": stub.to_str().unwrap() } } }
            })
            .to_string(),
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let result = preflight_validate_and_probe(dir.path(), &cfg);
        assert!(
            result.is_ok(),
            "clean config must pass validation + probe: {result:?}"
        );
    }

    #[test]
    fn test_preflight_rejects_invalid_models_config_before_probe() {
        // AC#7: validation runs (and fails) BEFORE any binary probe. A codex
        // `xhigh` effort is a policy violation with no binary to probe — a
        // probe-only preflight would wave it through.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            serde_json::json!({
                "models": { "providers": { "codex": { "enabled": true, "effort": { "high": "xhigh" } } } }
            })
            .to_string(),
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let err = preflight_validate_and_probe(dir.path(), &cfg)
            .expect_err("invalid models config must be rejected by validation");
        let msg = format!("{err}");
        assert!(
            msg.contains("xhigh"),
            "validation error must name the offending value: {msg}"
        );
    }

    #[test]
    fn test_preflight_errors_when_enabled_provider_binary_missing() {
        // AC#7: probes fire only for ENABLED providers and surface a missing
        // binary as Err. Claude (stub) passes; codex is enabled with a cliBinary
        // pointing at a nonexistent path → probe Err naming codex.
        let _guard = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _claude_env = EnvGuard::remove("CLAUDE_BINARY");
        let _codex_env = EnvGuard::remove("CODEX_BINARY");
        let dir = tempfile::tempdir().unwrap();
        let stub = make_executable_stub(dir.path(), "claude-stub");
        let missing = dir.path().join("nonexistent-codex-binary");
        fs::write(
            dir.path().join("config.json"),
            serde_json::json!({
                "models": { "providers": {
                    "claude": { "cliBinary": stub.to_str().unwrap() },
                    "codex": { "enabled": true, "cliBinary": missing.to_str().unwrap() }
                } }
            })
            .to_string(),
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let err = preflight_validate_and_probe(dir.path(), &cfg)
            .expect_err("missing codex binary must return Err");
        let msg = format!("{err}").to_ascii_lowercase();
        assert!(msg.contains("codex"), "probe error must name codex: {msg}");
    }

    // ============ FEAT-006: strict provider parser + provider-only Codex ============

    /// AC (positive): the provider-only Codex rule from V2 is preserved —
    /// `{provider:"codex", model:""}` validates OK. Removing this allowance
    /// would re-introduce a hand-written model-id that the dispatcher's
    /// provider-hint routing now makes unnecessary.
    #[test]
    fn test_validate_accepts_codex_provider_with_blank_model() {
        let mut by_type = HashMap::new();
        by_type.insert(
            "spike".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: "".to_string(),
                ..Default::default()
            },
        );
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_task_type: by_type,
                ..Default::default()
            }),
            ..Default::default()
        };
        validate_runner_routing_config(&cfg).expect("codex provider-only must validate");
    }

    /// AC (negative): a non-Codex route with a blank model is still rejected.
    /// The Codex allowance must be provider-specific — no quiet widening to
    /// other providers.
    #[test]
    fn test_validate_rejects_claude_provider_with_blank_model() {
        let mut by_type = HashMap::new();
        by_type.insert(
            "review".to_string(),
            RunnerSpec {
                provider: "claude".to_string(),
                model: "".to_string(),
                ..Default::default()
            },
        );
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_task_type: by_type,
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_runner_routing_config(&cfg).expect_err("blank model must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("model must not be blank") || msg.contains(".model"),
            "error must call out the blank-model rule: {msg}",
        );
    }

    /// Known-bad (the load-bearing AC): an unknown provider typo MUST hard-fail
    /// at validation. With the old `Option`-returning parser, `"groq"` would
    /// silently produce `None`, and the dispatcher would route the task to
    /// Claude (the silent-fallback footgun). The strict parser surfaces it.
    #[test]
    fn test_validate_rejects_unknown_provider_typo() {
        for bad in ["groq", "openai", "codex-cli", "anthropic"] {
            let mut by_id = HashMap::new();
            by_id.insert(
                "TYPO-".to_string(),
                RunnerSpec {
                    provider: bad.to_string(),
                    model: "some-model".to_string(),
                    ..Default::default()
                },
            );
            let cfg = ProjectConfig {
                primary_runner: Some(PrimaryRunnerConfig {
                    by_id_prefix: by_id,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let err = validate_runner_routing_config(&cfg)
                .expect_err(&format!("typo {bad:?} must reject"));
            let msg = format!("{err}");
            assert!(
                msg.contains(bad)
                    && msg.contains("claude")
                    && msg.contains("grok")
                    && msg.contains("codex"),
                "error must name the bad provider {bad:?} AND the allowed set: {msg}",
            );
            // Same value must NOT cause a Codex fallback to fire — verifies
            // the validation hard-fail comes BEFORE any model-based fallback.
            assert!(
                !msg.to_ascii_lowercase().contains("blank"),
                "the typo path must surface the unknown-provider error, not the blank-model rule: {msg}",
            );
        }
    }

    #[test]
    fn test_validate_rejects_unknown_baseline_tier() {
        let mut tiers = HashMap::new();
        tiers.insert(
            "superopus".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), tiers);
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                baseline_tier_routes,
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_runner_routing_config(&cfg).expect_err("unknown tier must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("baselineTierRoutes.FEAT.superopus") && msg.contains("low"),
            "error must name the bad tier and allowed tiers: {msg}"
        );
    }

    #[test]
    fn test_validate_rejects_conflicting_overlapping_by_id_prefix_routes() {
        use crate::loop_engine::model::OPUS_MODEL;
        let mut by_id_prefix = HashMap::new();
        by_id_prefix.insert(
            "MILESTONE".to_string(),
            RunnerSpec {
                provider: "claude".to_string(),
                model: OPUS_MODEL.to_string(),
                ..Default::default()
            },
        );
        by_id_prefix.insert(
            "MILESTONE-FINAL".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_id_prefix,
                ..Default::default()
            }),
            ..Default::default()
        };

        let err = validate_runner_routing_config(&cfg)
            .expect_err("conflicting overlapping prefixes must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("byIdPrefix")
                && msg.contains("MILESTONE")
                && msg.contains("MILESTONE-FINAL"),
            "error must name both conflicting prefixes: {msg}",
        );
    }

    #[test]
    fn test_validate_allows_same_spec_overlapping_by_id_prefix_routes() {
        use crate::loop_engine::model::OPUS_MODEL;
        let same_spec = RunnerSpec {
            provider: "claude".to_string(),
            model: OPUS_MODEL.to_string(),
            ..Default::default()
        };
        let mut by_id_prefix = HashMap::new();
        by_id_prefix.insert("FIX".to_string(), same_spec.clone());
        by_id_prefix.insert("CODE-FIX".to_string(), same_spec.clone());
        by_id_prefix.insert("IMPL-FIX".to_string(), same_spec.clone());
        by_id_prefix.insert("WIRE-FIX".to_string(), same_spec);
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_id_prefix,
                ..Default::default()
            }),
            ..Default::default()
        };

        validate_runner_routing_config(&cfg)
            .expect("same-spec overlapping prefixes must remain valid");
    }

    #[test]
    fn test_validate_rejects_conflicting_overlapping_baseline_tier_routes() {
        let mut feat_tiers = HashMap::new();
        feat_tiers.insert(
            "high".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let mut special_tiers = HashMap::new();
        special_tiers.insert(
            "high".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-build".to_string(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), feat_tiers);
        baseline_tier_routes.insert("FEAT-SPECIAL".to_string(), special_tiers);
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                baseline_tier_routes,
                ..Default::default()
            }),
            ..Default::default()
        };

        let err = validate_runner_routing_config(&cfg)
            .expect_err("conflicting overlapping baseline tier prefixes must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("baselineTierRoutes")
                && msg.contains("FEAT")
                && msg.contains("FEAT-SPECIAL")
                && msg.contains("high"),
            "error must name both prefixes and the conflicting tier: {msg}",
        );
    }

    #[test]
    fn test_validate_allows_overlapping_baseline_prefixes_with_disjoint_tiers() {
        let mut feat_tiers = HashMap::new();
        feat_tiers.insert(
            "standard".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-build".to_string(),
                ..Default::default()
            },
        );
        let mut special_tiers = HashMap::new();
        special_tiers.insert(
            "high".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), feat_tiers);
        baseline_tier_routes.insert("FEAT-SPECIAL".to_string(), special_tiers);
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                baseline_tier_routes,
                ..Default::default()
            }),
            ..Default::default()
        };

        validate_runner_routing_config(&cfg)
            .expect("overlapping baseline prefixes with disjoint tiers must remain valid");
    }

    #[test]
    fn test_validate_rejects_conflicting_baseline_tier_aliases() {
        let mut feat_tiers = HashMap::new();
        feat_tiers.insert(
            "high".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        feat_tiers.insert(
            "opus".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-build".to_string(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), feat_tiers);
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                baseline_tier_routes,
                ..Default::default()
            }),
            ..Default::default()
        };

        let err = validate_runner_routing_config(&cfg)
            .expect_err("conflicting baseline tier aliases must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("baselineTierRoutes") && msg.contains("high") && msg.contains("opus"),
            "error must name both alias tier keys: {msg}",
        );
    }

    /// AC (WS-2.2): a route model that starts with `-` (after trim) is rejected
    /// with an error naming the offending route key, so it cannot be argv-flag-
    /// confused on the child CLI. Codex routes with an empty model are still valid.
    #[test]
    fn test_validate_rejects_model_starting_with_dash() {
        // Positive: dash-prefixed model on a Grok route must be rejected.
        let mut by_id = HashMap::new();
        by_id.insert(
            "FEAT-".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "--flag-injection".to_string(),
                ..Default::default()
            },
        );
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_id_prefix: by_id,
                ..Default::default()
            }),
            ..Default::default()
        };
        let err =
            validate_runner_routing_config(&cfg).expect_err("dash-prefixed model must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("byIdPrefix.FEAT-.model") && msg.contains('-'),
            "error must name the offending route key and the offending value: {msg}",
        );

        // Negative: Codex route with empty model must still validate (the check
        // only fires on a non-empty model starting with `-`).
        let mut by_type = HashMap::new();
        by_type.insert(
            "spike".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let cfg_codex = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_task_type: by_type,
                ..Default::default()
            }),
            ..Default::default()
        };
        validate_runner_routing_config(&cfg_codex)
            .expect("codex route with empty model must not trigger the dash guard");
    }

    /// CONTRACT: `EffectiveRunnerInput` field names match the struct in
    /// `engine.rs` exactly — `model` and `provider_hint`. A rename in
    /// `engine.rs` without a matching rename here (or downstream) would
    /// break the production drift guard. Grep the engine source rather
    /// than re-importing so this test cannot be silently weakened by an
    /// `impl Into` shim.
    #[test]
    fn test_effective_runner_input_field_names_in_engine_rs() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/loop_engine/engine.rs"
        ))
        .expect("engine.rs must be readable for the CONTRACT check");
        assert!(
            src.contains("pub struct EffectiveRunnerInput"),
            "engine.rs must define `pub struct EffectiveRunnerInput`",
        );
        assert!(
            src.contains("pub model: Option<&'a str>"),
            "EffectiveRunnerInput::model field name/type must be `pub model: Option<&'a str>`",
        );
        assert!(
            src.contains("pub provider_hint: Option<model::Provider>"),
            "EffectiveRunnerInput::provider_hint field name/type must be \
             `pub provider_hint: Option<model::Provider>`",
        );
    }

    // ---- write_review_model tests ----

    #[test]
    fn test_write_review_model_sets_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();
        write_review_model(dir.path(), Some("grok-build")).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains("\"reviewModel\""), "key should be set");
        assert!(raw.contains("grok-build"), "value should be present");
        // load-bearing: unrelated key must survive
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(
            raw.contains("Bash(docker:*)"),
            "additionalAllowedTools value lost"
        );
    }

    #[test]
    fn test_write_review_model_removes_key_when_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"reviewModel":"grok-build"}"#,
        )
        .unwrap();
        write_review_model(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(!raw.contains("reviewModel"), "key should be removed: {raw}");
    }

    #[test]
    fn test_write_review_model_none_on_absent_key_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        write_review_model(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("reviewModel"),
            "key should stay absent: {raw}"
        );
    }

    #[test]
    fn test_write_review_model_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        write_review_model(dir.path(), Some("grok-build")).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.review_model.as_deref(), Some("grok-build"));
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("\"version\""),
            "version key must be present on new file"
        );
    }

    #[test]
    fn test_write_review_model_preserves_all_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"embeddingModel":"x","ollamaUrl":"http://x","rerankerModel":"m"}"#,
        )
        .unwrap();
        write_review_model(dir.path(), Some("grok-build")).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(
            raw.contains("Bash(docker:*)"),
            "additionalAllowedTools value lost"
        );
        assert!(raw.contains("embeddingModel"), "embeddingModel lost");
        assert!(raw.contains("ollamaUrl"), "ollamaUrl lost");
        assert!(raw.contains("rerankerModel"), "rerankerModel lost");
        assert!(raw.contains("grok-build"), "reviewModel not set");
    }

    #[test]
    fn test_write_review_model_malformed_json_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "not json at all").unwrap();
        let result = write_review_model(dir.path(), Some("grok-build"));
        assert!(result.is_err(), "malformed JSON must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("config.json"),
            "error must name the file: {msg}"
        );
    }

    // ---- write_fallback_runner tests ----

    #[test]
    fn test_write_fallback_runner_sets_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();
        let cfg = FallbackRunnerConfig {
            enabled: true,
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            cli_binary: None,
            runtime_error_threshold: 2,
        };
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains("\"fallbackRunner\""), "key should be set");
        assert!(raw.contains("\"enabled\""), "enabled field must be present");
        // load-bearing: unrelated key must survive
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(
            raw.contains("Bash(docker:*)"),
            "additionalAllowedTools value lost"
        );
        let config = read_project_config(dir.path());
        let fr = config
            .fallback_runner
            .expect("fallbackRunner should be Some");
        assert!(fr.enabled);
        assert_eq!(fr.provider, "grok");
        assert_eq!(fr.model, "grok-build");
        assert!(fr.cli_binary.is_none());
    }

    #[test]
    fn test_write_fallback_runner_with_cli_binary() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FallbackRunnerConfig {
            enabled: true,
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            cli_binary: Some("/usr/local/bin/grok".to_string()),
            runtime_error_threshold: 3,
        };
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let config = read_project_config(dir.path());
        let fr = config
            .fallback_runner
            .expect("fallbackRunner should be Some");
        assert_eq!(fr.cli_binary.as_deref(), Some("/usr/local/bin/grok"));
        assert_eq!(fr.runtime_error_threshold, 3);
    }

    #[test]
    fn test_write_fallback_runner_removes_key_when_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"fallbackRunner":{"enabled":true,"provider":"grok","model":"grok-build","runtimeErrorThreshold":2}}"#,
        )
        .unwrap();
        write_fallback_runner(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("fallbackRunner"),
            "key should be removed: {raw}"
        );
    }

    #[test]
    fn test_write_fallback_runner_none_on_absent_key_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        write_fallback_runner(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("fallbackRunner"),
            "key should stay absent: {raw}"
        );
    }

    #[test]
    fn test_write_fallback_runner_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FallbackRunnerConfig::default();
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let config = read_project_config(dir.path());
        assert!(
            config.fallback_runner.is_some(),
            "fallbackRunner should be set"
        );
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("\"version\""),
            "version key must be present on new file"
        );
    }

    #[test]
    fn test_write_fallback_runner_malformed_json_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{{bad json").unwrap();
        let result = write_fallback_runner(dir.path(), Some(&FallbackRunnerConfig::default()));
        assert!(result.is_err(), "malformed JSON must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("config.json"),
            "error must name the file: {msg}"
        );
    }

    #[test]
    fn test_write_fallback_runner_cli_binary_none_not_serialized_as_null() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FallbackRunnerConfig {
            enabled: false,
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            cli_binary: None,
            runtime_error_threshold: 2,
        };
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("\"cliBinary\""),
            "None cli_binary should be omitted, not null"
        );
    }

    // ============ models/routing config (FR-001) ============

    /// A valid baseline: a single enabled provider with one model tier.
    fn enabled_provider_models(provider: &str, model: &str) -> ModelsConfig {
        let mut providers = HashMap::new();
        providers.insert(
            provider.to_string(),
            ProviderConfig {
                enabled: true,
                tiers: HashMap::from([("standard".to_string(), Some(model.to_string()))]),
                effort: HashMap::new(),
                fallback: None,
                cli_binary: None,
            },
        );
        ModelsConfig {
            primary_provider: provider.to_string(),
            anchor: "standard".to_string(),
            providers,
        }
    }

    #[test]
    fn models_builtin_default_deserializes_canonical_shape() {
        // FR-001 defaults: claude enabled, grok/codex disabled, anchor=standard,
        // primaryProvider=claude. Round-trips through serde verbatim.
        let cfg = ModelsConfig::builtin_default();
        assert_eq!(cfg.primary_provider, "claude");
        assert_eq!(cfg.anchor, "standard");
        assert!(cfg.providers["claude"].enabled);
        assert!(!cfg.providers["grok"].enabled);
        assert!(!cfg.providers["codex"].enabled);
        let value = serde_json::to_value(&cfg).unwrap();
        let back: ModelsConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            cfg, back,
            "ModelsConfig must round-trip through JSON verbatim"
        );
        // The default config + default routing is itself valid.
        assert!(validate_models_config(&cfg, &RoutingConfig::default()).is_ok());
    }

    #[test]
    fn merge_models_config_field_wise_optin() {
        // {"providers":{"grok":{"enabled":true}}} is a COMPLETE opt-in.
        let user = serde_json::json!({ "providers": { "grok": { "enabled": true } } });
        let merged = merge_models_config(Some(&user)).unwrap();
        assert!(merged.providers["grok"].enabled);
        assert_eq!(
            merged.providers["grok"].tiers.get("standard"),
            Some(&Some("grok-build".to_string())),
            "grok keeps its default ladder under field-wise merge"
        );
        assert!(merged.providers["claude"].enabled, "claude untouched");
        // None / explicit null → pure default.
        assert_eq!(
            merge_models_config(None).unwrap(),
            ModelsConfig::builtin_default()
        );
        assert_eq!(
            merge_models_config(Some(&serde_json::Value::Null)).unwrap(),
            ModelsConfig::builtin_default()
        );
    }

    #[test]
    fn validate_rejects_legacy_alias_and_unknown_tier_keys() {
        let mut models = enabled_provider_models("claude", OPUS_MODEL);
        models
            .providers
            .get_mut("claude")
            .unwrap()
            .tiers
            .insert("opus".to_string(), Some(OPUS_MODEL.to_string()));
        let errs = validate_models_config(&models, &RoutingConfig::default()).unwrap_err();
        // Legacy alias rejected (ambiguity may also fire — but the tier-key
        // error must be present and name the accepted set).
        assert!(
            errs.iter()
                .any(|e| e.contains("opus") && e.contains("cost-efficient")),
            "legacy alias tier key must be rejected naming the accepted set: {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_disabled_primary_provider() {
        let mut models = enabled_provider_models("claude", OPUS_MODEL);
        models.providers.get_mut("claude").unwrap().enabled = false;
        let errs = validate_models_config(&models, &RoutingConfig::default()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("primaryProvider") && e.contains("disabled")),
            "disabled primaryProvider must be rejected: {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_ambiguous_reverse_model_lookup() {
        let mut models = enabled_provider_models("claude", OPUS_MODEL);
        // Two tiers → the SAME model id → reverse lookup is ambiguous.
        models
            .providers
            .get_mut("claude")
            .unwrap()
            .tiers
            .insert("frontier".to_string(), Some(OPUS_MODEL.to_string()));
        let errs = validate_models_config(&models, &RoutingConfig::default()).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("ambiguous reverse lookup")),
            "duplicate model across tiers must be rejected: {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_codex_effort_xhigh_by_policy() {
        let mut providers = HashMap::new();
        providers.insert(
            "claude".to_string(),
            ProviderConfig {
                enabled: true,
                tiers: HashMap::from([("standard".to_string(), Some(OPUS_MODEL.to_string()))]),
                ..Default::default()
            },
        );
        providers.insert(
            "codex".to_string(),
            ProviderConfig {
                enabled: true,
                tiers: HashMap::from([("standard".to_string(), None)]),
                effort: HashMap::from([("high".to_string(), Some("xhigh".to_string()))]),
                ..Default::default()
            },
        );
        let models = ModelsConfig {
            primary_provider: "claude".to_string(),
            anchor: "standard".to_string(),
            providers,
        };
        let errs = validate_models_config(&models, &RoutingConfig::default()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("xhigh") && e.contains("policy")),
            "codex xhigh must be rejected by policy: {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_fallback_to_self_or_disabled() {
        // Fallback to self.
        let mut models = enabled_provider_models("claude", OPUS_MODEL);
        models.providers.get_mut("claude").unwrap().fallback = Some("claude".to_string());
        let errs = validate_models_config(&models, &RoutingConfig::default()).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("fall back to itself")),
            "self-fallback must be rejected: {errs:?}"
        );
        // Fallback to a disabled provider (grok present-but-disabled).
        let mut models2 = ModelsConfig::builtin_default();
        models2.providers.get_mut("claude").unwrap().fallback = Some("grok".to_string());
        let errs2 = validate_models_config(&models2, &RoutingConfig::default()).unwrap_err();
        assert!(
            errs2
                .iter()
                .any(|e| e.contains("fallback") && e.contains("not an enabled provider")),
            "fallback to disabled provider must be rejected: {errs2:?}"
        );
    }

    #[test]
    fn validate_rejects_routes_referencing_disabled_providers() {
        let models = enabled_provider_models("claude", OPUS_MODEL);
        let routing = RoutingConfig {
            by_id_prefix: HashMap::from([(
                "FEAT-".to_string(),
                RouteSpec {
                    provider: "grok".to_string(),
                    tier: None,
                },
            )]),
            ..Default::default()
        };
        let errs = validate_models_config(&models, &routing).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("byIdPrefix") && e.contains("not enabled")),
            "route to disabled provider must be rejected: {errs:?}"
        );
    }

    #[test]
    fn validate_rejects_premature_review_cascade_key() {
        let models = enabled_provider_models("claude", OPUS_MODEL);
        let routing = RoutingConfig {
            review_cascade: Some(serde_json::json!({ "stages": [] })),
            ..Default::default()
        };
        let errs = validate_models_config(&models, &routing).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("reviewCascade") && e.contains("not yet supported")),
            "reviewCascade must be rejected as deferred: {errs:?}"
        );
    }

    #[test]
    fn detect_legacy_model_keys_names_each_present_key() {
        let config = serde_json::json!({
            "defaultModel": "x",
            "reviewModel": "y",
            "primaryRunner": {},
            "fallbackRunner": {},
            "models": {},
            "additionalAllowedTools": []
        });
        assert_eq!(
            detect_legacy_model_keys(&config),
            vec![
                "defaultModel",
                "reviewModel",
                "primaryRunner",
                "fallbackRunner"
            ]
        );
        // A clean config returns an empty vec; a non-object returns empty.
        assert!(detect_legacy_model_keys(&serde_json::json!({ "models": {} })).is_empty());
        assert!(detect_legacy_model_keys(&serde_json::json!([1, 2, 3])).is_empty());
    }

    #[test]
    fn probe_skips_disabled_providers() {
        // A disabled provider with a bogus binary must NOT trip the probe —
        // enabled-gated. Only a disabled grok is present; nothing to probe.
        let mut providers = HashMap::new();
        providers.insert(
            "grok".to_string(),
            ProviderConfig {
                enabled: false,
                tiers: HashMap::from([("standard".to_string(), Some("grok-build".to_string()))]),
                cli_binary: Some("/tmp/task-mgr-test-nonexistent-binary-xyz".to_string()),
                ..Default::default()
            },
        );
        let models = ModelsConfig {
            primary_provider: "grok".to_string(),
            anchor: "standard".to_string(),
            providers,
        };
        let resolved =
            crate::loop_engine::model::resolve_models_config(&models, &RoutingConfig::default());
        assert!(
            probe_enabled_provider_binaries(&resolved).is_ok(),
            "disabled provider with a missing binary must be skipped"
        );
    }

    #[test]
    fn probe_errors_on_enabled_provider_missing_binary() {
        use crate::loop_engine::test_utils::GROK_BINARY_MUTEX;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // GROK_BINARY unset so the probe checks the explicit (bogus) cliBinary.
        unsafe { std::env::remove_var("GROK_BINARY") };
        let mut providers = HashMap::new();
        providers.insert(
            "grok".to_string(),
            ProviderConfig {
                enabled: true,
                tiers: HashMap::from([("standard".to_string(), Some("grok-build".to_string()))]),
                cli_binary: Some("/tmp/task-mgr-test-nonexistent-grok-xyz".to_string()),
                ..Default::default()
            },
        );
        let models = ModelsConfig {
            primary_provider: "grok".to_string(),
            anchor: "standard".to_string(),
            providers,
        };
        let resolved =
            crate::loop_engine::model::resolve_models_config(&models, &RoutingConfig::default());
        let result = probe_enabled_provider_binaries(&resolved);
        assert!(
            result.is_err(),
            "enabled grok with a missing binary must error"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("grok"),
            "probe error must name the provider/binary; got {msg}"
        );
    }
}
