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
//! `escalate_model` moves the model up one tier on repeat crash / consecutive
//! failure (called from `engine::check_crash_escalation` and
//! `engine::escalate_task_model_if_needed`). `downgrade_effort` moves effort
//! down one tier on `PromptTooLong` (called from the `PromptTooLong` branch
//! in `engine.rs`). A task that starts at (Sonnet, `xhigh`) and keeps crashing
//! can therefore land at (Opus, `high`); this is intentional — `max` effort
//! was retired for overflowing context and the `xhigh → high` step is the
//! same safety valve for `xhigh`.
//!
//! When effort downgrade is exhausted (already at `high`), `to_1m_model`
//! provides a second escape hatch: escalate to the 1M-context variant of
//! the current model (currently Opus only). This gives the task a larger
//! context window without changing effort.

use crate::loop_engine::project_config::{PrimaryRunnerConfig, RunnerSpec};

/// Well-known model identifiers.
pub const OPUS_MODEL: &str = "claude-opus-4-8";
pub const OPUS_MODEL_1M: &str = "claude-opus-4-8[1m]";
pub const SONNET_MODEL: &str = "claude-sonnet-4-6";
pub const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";

/// LLM provider classification.
///
/// Computed from a model id by [`provider_for_model`] (token-equality on the
/// lowercased, hyphen-split model string). Used to pick the right
/// `RunnerKind` and to short-circuit Claude-only helpers like
/// [`escalate_model`] when the active model belongs to a different provider.
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

/// Resolved model plus optional explicit provider intent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedExecutionTarget {
    pub model: Option<String>,
    pub provider_hint: Option<Provider>,
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
pub fn is_review_class(id: &str) -> bool {
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
/// Scaling: low difficulty → medium effort, medium → high, high → xhigh.
/// Unset / unknown difficulty → no `--effort` flag (CLI default applies).
///
/// The ladder intentionally stops below `max`: `max` consistently overshot the
/// model context budget on long iterations (observed repeated
/// `Prompt is too long` failures on Opus 4.7). On overflow, `downgrade_effort`
/// steps `xhigh → high`; `high` is the floor.
pub const EFFORT_FOR_DIFFICULTY: &[(&str, &str)] =
    &[("low", "medium"), ("medium", "high"), ("high", "xhigh")];

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

/// Model tier ordering for comparison.
///
/// Variants are ordered from lowest to highest capability/cost.
/// `Default` represents an unknown or unspecified model.
///
/// Note: `model_tier` uses **substring** matching (`.contains("opus")` etc.)
/// because model variant suffixes like `[1m]` must still map to the correct
/// tier. `provider_for_model` deliberately uses **token equality** on `-`
/// splits to avoid mis-routing Groq Inc. models (`groq-llama-3`, which
/// contains "groq" but not the token "grok") to the xAI Grok runner. The
/// two functions' matching strategies are intentionally different — do not
/// "unify" them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelTier {
    /// No model specified or unrecognized model string
    Default,
    /// Fastest and cheapest tier
    Haiku,
    /// Balanced capability/cost tier
    Sonnet,
    /// Highest capability tier
    Opus,
}

/// Determine the tier of a model string by case-insensitive substring matching.
///
/// Returns `ModelTier::Default` for `None` or unrecognized strings.
pub fn model_tier(model: Option<&str>) -> ModelTier {
    match model {
        None => ModelTier::Default,
        Some(s) => {
            let lower = s.to_lowercase();
            if lower.contains("opus") {
                ModelTier::Opus
            } else if lower.contains("sonnet") {
                ModelTier::Sonnet
            } else if lower.contains("haiku") {
                ModelTier::Haiku
            } else {
                ModelTier::Default
            }
        }
    }
}

/// Parse a baseline-tier key from `primaryRunner.baselineTierRoutes`.
///
/// Accepted keys are the stable provider-neutral tier names used in config:
/// `low`, `standard`, and `high`. The older Claude-family keys (`haiku`,
/// `sonnet`, `opus`) remain accepted as aliases for backward compatibility.
/// `default` is intentionally rejected because an unknown/no-model baseline
/// is not a useful routing policy input.
pub fn parse_baseline_tier_key(s: &str) -> Result<ModelTier, String> {
    let trimmed = s.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "low" | "haiku" => Ok(ModelTier::Haiku),
        "standard" | "sonnet" => Ok(ModelTier::Sonnet),
        "high" | "opus" => Ok(ModelTier::Opus),
        _ => Err(format!(
            "unknown baseline tier {trimmed:?} (expected one of: low, standard, high)"
        )),
    }
}

/// Inputs to [`resolve_task_model`]. Built with `..Default::default()` so
/// callers only name the fields they have; absent fields contribute nothing to
/// the resolution chain. Using a struct over positional args avoids the
/// silent-swap footgun from a 5-parameter signature.
#[derive(Debug, Default, Clone, Copy)]
pub struct ModelResolutionContext<'a> {
    /// Model explicitly set on the task (empty/whitespace-only normalized to `None`).
    pub task_model: Option<&'a str>,
    /// Task difficulty — triggers `OPUS_MODEL` escalation when equal to `"high"`.
    pub difficulty: Option<&'a str>,
    /// Default from PRD metadata (`prd_metadata.default_model`).
    pub prd_default: Option<&'a str>,
    /// Default from the per-project config (`.task-mgr/config.json`).
    pub project_default: Option<&'a str>,
    /// Default from the per-user config (`$XDG_CONFIG_HOME/task-mgr/config.json`).
    pub user_default: Option<&'a str>,
    /// Task ID used for `byIdPrefix` matching in the primary runner config.
    /// The 8-hex project prefix is stripped before matching so both bare IDs
    /// (`REVIEW-001`) and prefixed IDs (`8d71d1f7-REVIEW-001`) work identically.
    pub task_id: Option<&'a str>,
    /// Semantic task type (e.g. `"review"`, `"milestone"`, `"implementation"`).
    /// Matched against the `byTaskType` map in [`PrimaryRunnerConfig`].
    pub task_type: Option<&'a str>,
    /// Per-project primary runner config used for rung-2 routing before the
    /// `difficulty=high → OPUS_MODEL` rung. `None` → rung 2 is skipped and
    /// behaviour is byte-identical to the pre-primary-runner resolution chain.
    pub primary_runner: Option<&'a PrimaryRunnerConfig>,
}

/// Return the first matching [`RunnerSpec`] from a [`PrimaryRunnerConfig`] for
/// a given task identity pair `(task_id, task_type)`.
///
/// **Match priority** (highest → lowest):
/// 1. `byTaskType` — exact, case-sensitive match on `task_type`
/// 2. `byIdPrefix` — the map key matches a dash-delimited segment at the
///    start of the task ID body (after stripping the 8-hex project prefix).
///    The key is normalized by trimming a trailing `-`, so `"REVIEW"` and
///    `"REVIEW-"` behave identically, and a `-` boundary is required after
///    the prefix — so `"REVIEW-"` matches `REVIEW-001` but NOT `REVIEWER-001`.
///    Same dash-boundary semantics as `id_body_matches_prefix` in
///    `commands::next::selection` (kept as a local matcher rather than reused,
///    to avoid a `loop_engine` → `commands` layer dependency).
///
/// When both maps produce a match, `byTaskType` wins even if the specs differ.
/// `None` is returned only when neither map matches.
///
/// This is a pure function with no I/O. The `cfg` reference lifetime bounds
/// the returned `RunnerSpec` to the config it was drawn from.
pub fn primary_runner_match<'a>(
    cfg: &'a PrimaryRunnerConfig,
    task_id: Option<&str>,
    task_type: Option<&str>,
) -> Option<&'a RunnerSpec> {
    // byTaskType wins when it matches — checked first.
    if let Some(spec) = task_type.and_then(|tt| cfg.by_task_type.get(tt)) {
        return Some(spec);
    }
    // byIdPrefix: check if any registered prefix matches the task ID.
    if let Some(id) = task_id {
        let body = strip_prd_prefix(id);
        for (prefix, spec) in &cfg.by_id_prefix {
            if id_body_matches_prefix(body, prefix) {
                return Some(spec);
            }
        }
    }
    None
}

fn id_body_matches_prefix(body: &str, prefix: &str) -> bool {
    // Normalize the key (trim a trailing `-`) then require a `-`
    // boundary after the prefix segment, so a key like "REVIEW" cannot
    // false-match "REVIEWER-001".
    let needle = format!("{}-", prefix.trim_end_matches('-'));
    body.starts_with(&needle) || body.contains(&format!("-{needle}"))
}

/// Return a baseline-tier remap for a task ID and Claude baseline model.
///
/// `baselineTierRoutes` is intentionally checked after direct primary-runner
/// routes. It gives projects a way to say "for FEAT work, if the normal
/// baseline is high capability, use Codex; if standard capability, use Grok"
/// without overriding explicit per-task `model` fields.
pub fn primary_runner_baseline_tier_match<'a>(
    cfg: &'a PrimaryRunnerConfig,
    task_id: Option<&str>,
    baseline_model: Option<&str>,
) -> Option<&'a RunnerSpec> {
    let tier = model_tier(baseline_model);
    if tier == ModelTier::Default {
        return None;
    }
    let body = strip_prd_prefix(task_id?);
    for (prefix, tier_map) in &cfg.baseline_tier_routes {
        if !id_body_matches_prefix(body, prefix) {
            continue;
        }
        for (tier_key, spec) in tier_map {
            if parse_baseline_tier_key(tier_key).ok() == Some(tier) {
                return Some(spec);
            }
        }
    }
    None
}

/// Resolve which model a single task should use.
///
/// Precedence (highest to lowest):
/// 1. `task_model` — explicit model set on the task
/// 2. `primary_runner` match (`byTaskType` then `byIdPrefix`) → configured model
/// 3. `difficulty == "high"` (case-insensitive) → `OPUS_MODEL`
/// 4. `prd_default` — default from PRD metadata
/// 5. `project_default` — default from `.task-mgr/config.json`
/// 6. `user_default` — default from `$XDG_CONFIG_HOME/task-mgr/config.json`
/// 7. `None` — no preference
///
/// Rung 2 is skipped entirely when `primary_runner` is `None` — behaviour is
/// byte-identical to the pre-primary-runner chain in that case.
///
/// Empty / whitespace-only strings in any field are normalized to `None` so
/// missing config values don't override real ones.
pub fn resolve_task_model(ctx: &ModelResolutionContext<'_>) -> Option<String> {
    resolve_task_execution_target(ctx).model
}

/// Compute the baseline Claude model from difficulty + the three default rungs.
///
/// This is the single source of truth for the lower half of the routing
/// precedence chain (rungs 3-6 in [`resolve_task_execution_target`]):
///
/// 1. `difficulty == "high"` (ASCII case-insensitive) → `OPUS_MODEL`.
/// 2. Otherwise the first non-blank of `prd_default`, `project_default`,
///    `user_default` (in that order), each filtered through [`normalize`] so
///    empty / whitespace-only config values don't shadow a real one.
/// 3. All `None`/blank → `None`.
///
/// Total function: every input combination returns; it never panics. Recovery
/// (`recovery.rs`) and the primary resolution path both call this so a
/// recovering task derives the same baseline tier it was originally routed by.
pub fn compute_baseline_model(
    difficulty: Option<&str>,
    prd_default: Option<&str>,
    project_default: Option<&str>,
    user_default: Option<&str>,
) -> Option<String> {
    if difficulty.is_some_and(|d| d.eq_ignore_ascii_case("high")) {
        Some(OPUS_MODEL.to_string())
    } else {
        [prd_default, project_default, user_default]
            .into_iter()
            .find_map(|fallback| normalize(fallback).map(str::to_string))
    }
}

/// Resolve the model and any explicit provider intent for a single task.
///
/// Only a `primaryRunner` match produces `provider_hint`. All model-string
/// rungs remain provider-agnostic so v1 cannot accidentally route Codex from a
/// generic OpenAI-looking model name.
pub fn resolve_task_execution_target(ctx: &ModelResolutionContext<'_>) -> ResolvedExecutionTarget {
    // Rung 1: explicit model on the task.
    if let Some(m) = normalize(ctx.task_model) {
        return ResolvedExecutionTarget {
            model: Some(m.to_string()),
            provider_hint: None,
        };
    }
    // Rung 2: primary runner match (byTaskType wins over byIdPrefix).
    if let Some(spec) = ctx
        .primary_runner
        .and_then(|cfg| primary_runner_match(cfg, ctx.task_id, ctx.task_type))
    {
        return ResolvedExecutionTarget {
            model: normalize(Some(&spec.model)).map(str::to_string),
            // Config validation rejects unknown providers up-front via
            // `parse_config_provider`, so by the time we reach the
            // dispatcher a malformed string would have already exited
            // with an error. `.ok()` here means "if we somehow got past
            // validation with a malformed provider, leave the hint empty
            // and let `provider_for_model` classify" — a safe degrade.
            provider_hint: parse_config_provider(&spec.provider).ok(),
        };
    }

    let baseline_model = compute_baseline_model(
        ctx.difficulty,
        ctx.prd_default,
        ctx.project_default,
        ctx.user_default,
    );

    // Rung 3: baseline-tier remap after the normal Claude baseline is known.
    if let Some(spec) = ctx.primary_runner.and_then(|cfg| {
        primary_runner_baseline_tier_match(cfg, ctx.task_id, baseline_model.as_deref())
    }) {
        return ResolvedExecutionTarget {
            model: normalize(Some(&spec.model)).map(str::to_string),
            provider_hint: parse_config_provider(&spec.provider).ok(),
        };
    }

    if let Some(model) = baseline_model {
        return ResolvedExecutionTarget {
            model: Some(model),
            provider_hint: None,
        };
    }
    ResolvedExecutionTarget::default()
}

fn normalize(s: Option<&str>) -> Option<&str> {
    s.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() { None } else { Some(v) }
    })
}

/// Resolve the model for an iteration by selecting the highest-tier model
/// from a synergy cluster of tasks.
///
/// When multiple tasks run in the same iteration (synergy cluster),
/// the highest-tier model wins so that all tasks get adequate capability.
///
/// Note: `max_by_key` returns the **last** element when tiers are tied,
/// so the model from the last task in the slice wins among equal tiers.
///
/// Returns `None` if the slice is empty or all entries are `None`.
pub fn resolve_iteration_model(task_models: &[Option<String>]) -> Option<String> {
    task_models
        .iter()
        .filter_map(|m| m.as_deref())
        .max_by_key(|m| model_tier(Some(m)))
        .map(String::from)
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

/// Escalate a model to the next higher tier.
///
/// - haiku → sonnet
/// - sonnet → opus
/// - opus → opus (already at ceiling)
/// - None → None
/// - unknown/Default tier → None (cannot escalate unrecognized model)
pub fn escalate_model(model: Option<&str>) -> Option<String> {
    if provider_for_model(model) != Provider::Claude {
        return None;
    }
    match model {
        None => None,
        Some(m) => match model_tier(Some(m)) {
            ModelTier::Default => None,
            ModelTier::Haiku => Some(SONNET_MODEL.to_string()),
            ModelTier::Sonnet => Some(OPUS_MODEL.to_string()),
            ModelTier::Opus => Some(OPUS_MODEL.to_string()),
        },
    }
}

/// Escalate a sub-Opus model to the next tier below Opus's 1M ceiling.
///
/// This is the second rung of `PromptTooLong` recovery: once effort downgrade
/// is exhausted (the `high` floor is preserved — effort never drops below `high`),
/// escalate the model itself before reaching for `to_1m_model`. Mirrors
/// `escalate_model` but stops at Opus instead of looping at it, so the caller
/// can distinguish "moved up a tier" from "already at ceiling".
///
/// **High effort floor invariant**: `downgrade_effort` stops at `high`; when it
/// returns `None` the loop calls this function instead of dropping effort further.
/// The effort level is never lowered past `high` — model escalation is the next
/// escape hatch, not a lower effort tier.
///
/// - haiku → sonnet
/// - sonnet → opus
/// - opus (incl. 1M variant) → None (already at ceiling)
/// - None / unknown tier → None
pub fn escalate_below_opus(model: Option<&str>) -> Option<&'static str> {
    if provider_for_model(model) != Provider::Claude {
        return None;
    }
    match model {
        None => None,
        Some(m) => match model_tier(Some(m)) {
            ModelTier::Haiku => Some(SONNET_MODEL),
            ModelTier::Sonnet => Some(OPUS_MODEL),
            ModelTier::Opus | ModelTier::Default => None,
        },
    }
}

/// Check whether a model string is already a 1M-context variant (contains `[1m]`).
pub fn is_1m_model(model: Option<&str>) -> bool {
    model.is_some_and(|m| m.to_lowercase().contains("[1m]"))
}

/// Return the 1M-context variant for a model, if one exists.
///
/// Currently only Opus has a 1M variant. Returns `None` if the model is
/// already 1M, not Opus-tier, or `None`.
pub fn to_1m_model(model: Option<&str>) -> Option<&'static str> {
    if provider_for_model(model) != Provider::Claude {
        return None;
    }
    match model {
        Some(m) if is_1m_model(Some(m)) => None,
        Some(m) if model_tier(Some(m)) == ModelTier::Opus => Some(OPUS_MODEL_1M),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ model_tier tests ============

    #[test]
    fn test_model_tier_opus_constants_and_substrings() {
        assert_eq!(model_tier(Some(OPUS_MODEL)), ModelTier::Opus);
        assert_eq!(model_tier(Some("claude-opus-4-7")), ModelTier::Opus);
        assert_eq!(model_tier(Some("some-opus-variant")), ModelTier::Opus);
    }

    #[test]
    fn test_model_tier_sonnet_constants_and_substrings() {
        assert_eq!(model_tier(Some(SONNET_MODEL)), ModelTier::Sonnet);
        assert_eq!(model_tier(Some("claude-sonnet-4-6")), ModelTier::Sonnet);
        assert_eq!(model_tier(Some("custom-sonnet-model")), ModelTier::Sonnet);
    }

    #[test]
    fn test_model_tier_haiku_constants_and_substrings() {
        assert_eq!(model_tier(Some(HAIKU_MODEL)), ModelTier::Haiku);
        assert_eq!(
            model_tier(Some("claude-haiku-4-5-20251001")),
            ModelTier::Haiku
        );
        assert_eq!(model_tier(Some("my-haiku-thing")), ModelTier::Haiku);
    }

    #[test]
    fn test_model_tier_none_returns_default() {
        assert_eq!(model_tier(None), ModelTier::Default);
    }

    #[test]
    fn test_model_tier_unknown_string_returns_default() {
        assert_eq!(model_tier(Some("gpt-4")), ModelTier::Default);
        assert_eq!(model_tier(Some("unknown-model")), ModelTier::Default);
        assert_eq!(model_tier(Some("")), ModelTier::Default);
    }

    #[test]
    fn test_model_tier_case_insensitive() {
        assert_eq!(model_tier(Some("Claude-OPUS-4")), ModelTier::Opus);
        assert_eq!(model_tier(Some("SONNET")), ModelTier::Sonnet);
        assert_eq!(model_tier(Some("Haiku")), ModelTier::Haiku);
    }

    /// AC: 'OPUS', 'Opus', 'claude-OPUS-4' all return Opus
    #[test]
    fn test_model_tier_case_variants_opus() {
        assert_eq!(model_tier(Some("OPUS")), ModelTier::Opus);
        assert_eq!(model_tier(Some("Opus")), ModelTier::Opus);
        assert_eq!(model_tier(Some("claude-OPUS-4")), ModelTier::Opus);
        assert_eq!(model_tier(Some("oPuS")), ModelTier::Opus);
    }

    #[test]
    fn test_model_tier_case_variants_sonnet() {
        assert_eq!(model_tier(Some("SONNET")), ModelTier::Sonnet);
        assert_eq!(model_tier(Some("Sonnet")), ModelTier::Sonnet);
        assert_eq!(model_tier(Some("claude-SONNET-4")), ModelTier::Sonnet);
        assert_eq!(model_tier(Some("sOnNeT")), ModelTier::Sonnet);
    }

    #[test]
    fn test_model_tier_case_variants_haiku() {
        assert_eq!(model_tier(Some("HAIKU")), ModelTier::Haiku);
        assert_eq!(model_tier(Some("Haiku")), ModelTier::Haiku);
        assert_eq!(model_tier(Some("claude-HAIKU-5")), ModelTier::Haiku);
        assert_eq!(model_tier(Some("hAiKu")), ModelTier::Haiku);
    }

    #[test]
    fn test_model_tier_whitespace_only_returns_default() {
        assert_eq!(model_tier(Some("  ")), ModelTier::Default);
        assert_eq!(model_tier(Some("\t")), ModelTier::Default);
    }

    #[test]
    fn test_model_tier_ordering() {
        assert!(ModelTier::Opus > ModelTier::Sonnet);
        assert!(ModelTier::Sonnet > ModelTier::Haiku);
        assert!(ModelTier::Haiku > ModelTier::Default);
    }

    // ============ compute_baseline_model tests ============

    #[test]
    fn compute_baseline_high_difficulty_short_circuits_to_opus() {
        // difficulty=high wins over every default, regardless of their values.
        assert_eq!(
            compute_baseline_model(
                Some("high"),
                Some(SONNET_MODEL),
                Some(HAIKU_MODEL),
                Some(HAIKU_MODEL),
            ),
            Some(OPUS_MODEL.to_string())
        );
        // ...and with no defaults set at all.
        assert_eq!(
            compute_baseline_model(Some("high"), None, None, None),
            Some(OPUS_MODEL.to_string())
        );
    }

    #[test]
    fn compute_baseline_high_difficulty_is_ascii_case_insensitive() {
        for variant in ["HIGH", "High", "hIgH"] {
            assert_eq!(
                compute_baseline_model(Some(variant), None, None, None),
                Some(OPUS_MODEL.to_string()),
                "difficulty={variant:?} should short-circuit to OPUS_MODEL"
            );
        }
    }

    #[test]
    fn compute_baseline_default_precedence_is_prd_then_project_then_user() {
        // All three present → prd wins.
        assert_eq!(
            compute_baseline_model(
                None,
                Some(SONNET_MODEL),
                Some(HAIKU_MODEL),
                Some(OPUS_MODEL),
            ),
            Some(SONNET_MODEL.to_string())
        );
        // prd absent → project wins over user.
        assert_eq!(
            compute_baseline_model(None, None, Some(HAIKU_MODEL), Some(OPUS_MODEL)),
            Some(HAIKU_MODEL.to_string())
        );
        // prd + project absent → user wins.
        assert_eq!(
            compute_baseline_model(None, None, None, Some(OPUS_MODEL)),
            Some(OPUS_MODEL.to_string())
        );
    }

    #[test]
    fn compute_baseline_all_none_returns_none() {
        assert_eq!(compute_baseline_model(None, None, None, None), None);
        // A non-high difficulty with no defaults is still None.
        assert_eq!(
            compute_baseline_model(Some("medium"), None, None, None),
            None
        );
    }

    #[test]
    fn compute_baseline_blank_defaults_skipped_via_normalize() {
        // Blank / whitespace-only earlier rungs are skipped, not propagated.
        assert_eq!(
            compute_baseline_model(None, Some("   "), Some("\t"), Some(HAIKU_MODEL)),
            Some(HAIKU_MODEL.to_string())
        );
        // All-blank defaults collapse to None.
        assert_eq!(
            compute_baseline_model(None, Some(""), Some("  "), Some("\t")),
            None
        );
    }

    // ============ resolve_task_model tests ============

    fn ctx<'a>() -> ModelResolutionContext<'a> {
        ModelResolutionContext::default()
    }

    #[test]
    fn test_resolve_task_model_task_wins_over_everything() {
        let result = resolve_task_model(&ModelResolutionContext {
            task_model: Some(HAIKU_MODEL),
            difficulty: Some("high"),
            prd_default: Some(SONNET_MODEL),
            project_default: Some(OPUS_MODEL),
            user_default: Some(OPUS_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_high_difficulty_beats_all_defaults() {
        let result = resolve_task_model(&ModelResolutionContext {
            difficulty: Some("high"),
            prd_default: Some(SONNET_MODEL),
            project_default: Some(HAIKU_MODEL),
            user_default: Some(HAIKU_MODEL),
            ..ctx()
        });
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "high difficulty must force OPUS_MODEL regardless of any default",
        );
    }

    #[test]
    fn test_resolve_task_model_prd_default_beats_both_configs() {
        let result = resolve_task_model(&ModelResolutionContext {
            prd_default: Some(SONNET_MODEL),
            project_default: Some(HAIKU_MODEL),
            user_default: Some(OPUS_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_project_default_beats_user() {
        let result = resolve_task_model(&ModelResolutionContext {
            project_default: Some(SONNET_MODEL),
            user_default: Some(OPUS_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_user_default_last_resort() {
        let result = resolve_task_model(&ModelResolutionContext {
            user_default: Some(HAIKU_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_all_none_returns_none() {
        assert_eq!(resolve_task_model(&ctx()), None);
    }

    /// Known-bad discriminator: "medium" difficulty must NOT escalate to opus.
    #[test]
    fn test_resolve_task_model_medium_does_not_escalate() {
        let result = resolve_task_model(&ModelResolutionContext {
            difficulty: Some("medium"),
            ..ctx()
        });
        assert_eq!(result, None);
    }

    /// Empty / whitespace-only strings anywhere in the chain are normalized
    /// to `None` so missing config values don't shadow real ones.
    #[test]
    fn test_resolve_task_model_empty_strings_are_ignored() {
        let result = resolve_task_model(&ModelResolutionContext {
            task_model: Some(""),
            prd_default: Some("   "),
            project_default: Some("\t"),
            user_default: Some(HAIKU_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    // ============ primary_runner_match tests ============

    /// Build a minimal PrimaryRunnerConfig for tests: "review" and "milestone"
    /// task types route to "grok-build" via byTaskType; "REVIEW-" id prefix also
    /// routes to "grok-build" via byIdPrefix.
    fn make_primary_runner_cfg() -> PrimaryRunnerConfig {
        use std::collections::HashMap;
        let grok_spec = RunnerSpec {
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            ..Default::default()
        };
        let mut by_task_type = HashMap::new();
        by_task_type.insert("review".to_string(), grok_spec.clone());
        by_task_type.insert("milestone".to_string(), grok_spec.clone());
        let mut by_id_prefix = HashMap::new();
        by_id_prefix.insert("REVIEW-".to_string(), grok_spec);
        PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type,
            by_id_prefix,
            ..Default::default()
        }
    }

    /// AC: taskType='review' + no task.model → resolves to 'grok-build'.
    #[test]
    fn test_primary_runner_task_type_review_resolves_grok() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            task_type: Some("review"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result, Some("grok-build".to_string()));
    }

    /// AC: taskType='milestone' → resolves to 'grok-build'.
    #[test]
    fn test_primary_runner_task_type_milestone_resolves_grok() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            task_type: Some("milestone"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result, Some("grok-build".to_string()));
    }

    /// AC: id='REVIEW-001' with taskType='implementation' → resolves to 'grok-build'
    /// (prefix match: byIdPrefix has "REVIEW-" key; byTaskType has no "implementation" key).
    #[test]
    fn test_primary_runner_id_prefix_match_wins_when_task_type_absent() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            task_id: Some("REVIEW-001"),
            task_type: Some("implementation"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result, Some("grok-build".to_string()));
    }

    /// AC: id='FEAT-001' with taskType='review' → resolves to 'grok-build'
    /// (taskType match wins even when byIdPrefix has no "FEAT-" key).
    #[test]
    fn test_primary_runner_task_type_wins_over_no_prefix_match() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            task_id: Some("FEAT-001"),
            task_type: Some("review"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result, Some("grok-build".to_string()));
    }

    /// AC: explicit task.model='claude-opus-4-7' on a review task → resolves to
    /// claude-opus-4-7 (rung 1 explicit wins over rung 2 primaryRunner).
    #[test]
    fn test_primary_runner_explicit_task_model_wins_over_primary_runner() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            task_model: Some(OPUS_MODEL),
            task_type: Some("review"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_primary_runner_codex_provider_only_preserves_provider_hint() {
        use std::collections::HashMap;
        let mut by_task_type = HashMap::new();
        by_task_type.insert(
            "review".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let cfg = PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type,
            by_id_prefix: HashMap::new(),
            ..Default::default()
        };
        let result = resolve_task_execution_target(&ModelResolutionContext {
            task_type: Some("review"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result.model, None);
        assert_eq!(result.provider_hint, Some(Provider::Codex));
    }

    #[test]
    fn test_explicit_task_model_suppresses_codex_primary_runner_hint() {
        use std::collections::HashMap;
        let mut by_task_type = HashMap::new();
        by_task_type.insert(
            "review".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let cfg = PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type,
            by_id_prefix: HashMap::new(),
            ..Default::default()
        };
        let result = resolve_task_execution_target(&ModelResolutionContext {
            task_model: Some(OPUS_MODEL),
            task_type: Some("review"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result.model, Some(OPUS_MODEL.to_string()));
        assert_eq!(result.provider_hint, None);
    }

    /// AC: difficulty='high' on a review task → resolves to 'grok-build'
    /// (primaryRunner rung 2 wins over difficulty=high rung 3).
    #[test]
    fn test_primary_runner_wins_over_difficulty_high() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            difficulty: Some("high"),
            task_type: Some("review"),
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(
            result,
            Some("grok-build".to_string()),
            "primaryRunner rung must precede difficulty=high escalation"
        );
    }

    #[test]
    fn test_primary_runner_baseline_tier_feat_high_routes_to_codex_hint() {
        use std::collections::HashMap;
        let mut tiers = HashMap::new();
        tiers.insert(
            "high".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                runtime_error_fallback: true,
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), tiers);
        let cfg = PrimaryRunnerConfig {
            baseline_tier_routes,
            ..Default::default()
        };

        let result = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some("8d71d1f7-FEAT-001"),
            difficulty: Some("high"),
            primary_runner: Some(&cfg),
            prd_default: Some(SONNET_MODEL),
            ..ctx()
        });

        assert_eq!(result.model, None);
        assert_eq!(result.provider_hint, Some(Provider::Codex));
    }

    #[test]
    fn test_primary_runner_baseline_tier_feat_standard_routes_to_grok() {
        use std::collections::HashMap;
        let mut tiers = HashMap::new();
        tiers.insert(
            "standard".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-build".to_string(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), tiers);
        let cfg = PrimaryRunnerConfig {
            baseline_tier_routes,
            ..Default::default()
        };

        let result = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some("FEAT-002"),
            primary_runner: Some(&cfg),
            prd_default: Some(SONNET_MODEL),
            ..ctx()
        });

        assert_eq!(result.model.as_deref(), Some("grok-build"));
        assert_eq!(result.provider_hint, Some(Provider::Grok));
    }

    #[test]
    fn test_explicit_task_model_wins_over_baseline_tier_route() {
        use std::collections::HashMap;
        let mut tiers = HashMap::new();
        tiers.insert(
            "high".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), tiers);
        let cfg = PrimaryRunnerConfig {
            baseline_tier_routes,
            ..Default::default()
        };

        let result = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some("FEAT-003"),
            task_model: Some(OPUS_MODEL),
            difficulty: Some("high"),
            primary_runner: Some(&cfg),
            ..ctx()
        });

        assert_eq!(result.model.as_deref(), Some(OPUS_MODEL));
        assert_eq!(result.provider_hint, None);
    }

    /// AC: primary_runner is None → behavior byte-identical to pre-primaryRunner
    /// chain; a review task type has no special routing.
    #[test]
    fn test_primary_runner_none_falls_through_to_defaults() {
        let result = resolve_task_model(&ModelResolutionContext {
            task_type: Some("review"),
            primary_runner: None,
            prd_default: Some(SONNET_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    /// AC: taskType='feature' AND id='FEAT-001' → no match in either map;
    /// default behaviour preserved (falls through to prd_default).
    #[test]
    fn test_primary_runner_no_match_falls_through_to_defaults() {
        let cfg = make_primary_runner_cfg();
        let result = resolve_task_model(&ModelResolutionContext {
            task_id: Some("FEAT-001"),
            task_type: Some("feature"),
            primary_runner: Some(&cfg),
            prd_default: Some(HAIKU_MODEL),
            ..ctx()
        });
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    /// AC: 8-hex project prefix is stripped before byIdPrefix matching.
    #[test]
    fn test_primary_runner_id_prefix_strips_prd_prefix() {
        let cfg = make_primary_runner_cfg();
        // Production ID: 8-hex prefix + REVIEW-001
        let result = resolve_task_model(&ModelResolutionContext {
            task_id: Some("8d71d1f7-REVIEW-001"),
            task_type: Some("implementation"), // no byTaskType match
            primary_runner: Some(&cfg),
            ..ctx()
        });
        assert_eq!(result, Some("grok-build".to_string()));
    }

    // ============ primary_runner_match unit tests ============

    #[test]
    fn test_primary_runner_match_task_type_wins_over_id_prefix() {
        use std::collections::HashMap;
        // Two specs with different models to verify which one wins.
        let mut by_task_type = HashMap::new();
        by_task_type.insert(
            "review".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-type-winner".to_string(),
                ..Default::default()
            },
        );
        let mut by_id_prefix = HashMap::new();
        by_id_prefix.insert(
            "REVIEW-".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-prefix-loser".to_string(),
                ..Default::default()
            },
        );
        let cfg = PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type,
            by_id_prefix,
            ..Default::default()
        };
        let spec = primary_runner_match(&cfg, Some("REVIEW-001"), Some("review"));
        assert_eq!(
            spec.map(|s| s.model.as_str()),
            Some("grok-type-winner"),
            "byTaskType must win over byIdPrefix when both match"
        );
    }

    #[test]
    fn test_primary_runner_match_falls_back_to_id_prefix_when_no_type_match() {
        let cfg = make_primary_runner_cfg();
        // task_type is not in byTaskType; id prefix IS in byIdPrefix.
        let spec = primary_runner_match(&cfg, Some("REVIEW-001"), Some("implementation"));
        assert_eq!(spec.map(|s| s.model.as_str()), Some("grok-build"));
    }

    #[test]
    fn test_primary_runner_match_returns_none_when_no_match() {
        let cfg = make_primary_runner_cfg();
        let spec = primary_runner_match(&cfg, Some("FEAT-001"), Some("feature"));
        assert!(spec.is_none());
    }

    #[test]
    fn test_primary_runner_match_none_task_id_and_type() {
        let cfg = make_primary_runner_cfg();
        assert!(primary_runner_match(&cfg, None, None).is_none());
    }

    // ============ Parameterized precedence table ============

    /// Exhaustive precedence combinations across all five inputs.
    /// Each row documents which field is expected to win.
    #[test]
    fn test_resolve_task_model_precedence_table() {
        struct Row<'a> {
            task_model: Option<&'a str>,
            difficulty: Option<&'a str>,
            prd_default: Option<&'a str>,
            project_default: Option<&'a str>,
            user_default: Option<&'a str>,
            expected: Option<&'a str>,
            reason: &'a str,
        }
        let cases = [
            Row {
                task_model: None,
                difficulty: None,
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: None,
                reason: "all None",
            },
            Row {
                task_model: None,
                difficulty: None,
                prd_default: Some(SONNET_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(SONNET_MODEL),
                reason: "only prd_default",
            },
            Row {
                task_model: None,
                difficulty: Some("high"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: Some(OPUS_MODEL),
                reason: "high diff forces opus",
            },
            Row {
                task_model: None,
                difficulty: Some("high"),
                prd_default: Some(SONNET_MODEL),
                project_default: Some(HAIKU_MODEL),
                user_default: Some(HAIKU_MODEL),
                expected: Some(OPUS_MODEL),
                reason: "high diff beats all defaults",
            },
            Row {
                task_model: None,
                difficulty: Some("medium"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: None,
                reason: "medium no fallback",
            },
            Row {
                task_model: None,
                difficulty: Some("medium"),
                prd_default: Some(HAIKU_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(HAIKU_MODEL),
                reason: "medium falls to prd",
            },
            Row {
                task_model: None,
                difficulty: Some("low"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: None,
                reason: "low no fallback",
            },
            Row {
                task_model: None,
                difficulty: Some("low"),
                prd_default: Some(SONNET_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(SONNET_MODEL),
                reason: "low falls to prd",
            },
            Row {
                task_model: Some(HAIKU_MODEL),
                difficulty: None,
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: Some(HAIKU_MODEL),
                reason: "task_model alone",
            },
            Row {
                task_model: Some(HAIKU_MODEL),
                difficulty: None,
                prd_default: Some(OPUS_MODEL),
                project_default: Some(OPUS_MODEL),
                user_default: Some(OPUS_MODEL),
                expected: Some(HAIKU_MODEL),
                reason: "task beats all defaults",
            },
            Row {
                task_model: Some(HAIKU_MODEL),
                difficulty: Some("high"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: Some(HAIKU_MODEL),
                reason: "task beats high",
            },
            Row {
                task_model: Some(HAIKU_MODEL),
                difficulty: Some("high"),
                prd_default: Some(OPUS_MODEL),
                project_default: Some(OPUS_MODEL),
                user_default: Some(OPUS_MODEL),
                expected: Some(HAIKU_MODEL),
                reason: "task wins over everything",
            },
            Row {
                task_model: Some(SONNET_MODEL),
                difficulty: Some("medium"),
                prd_default: Some(OPUS_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(SONNET_MODEL),
                reason: "task with medium",
            },
            Row {
                task_model: Some(OPUS_MODEL),
                difficulty: Some("low"),
                prd_default: Some(HAIKU_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(OPUS_MODEL),
                reason: "task with low",
            },
            Row {
                task_model: Some(""),
                difficulty: Some("high"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: Some(OPUS_MODEL),
                reason: "empty task falls to high",
            },
            Row {
                task_model: Some("  "),
                difficulty: None,
                prd_default: Some(SONNET_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(SONNET_MODEL),
                reason: "whitespace task falls to prd",
            },
            Row {
                task_model: Some(""),
                difficulty: None,
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: None,
                reason: "empty task, no fallbacks",
            },
            Row {
                task_model: None,
                difficulty: Some("High"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: Some(OPUS_MODEL),
                reason: "case 'High'",
            },
            Row {
                task_model: None,
                difficulty: Some("HIGH"),
                prd_default: None,
                project_default: None,
                user_default: None,
                expected: Some(OPUS_MODEL),
                reason: "case 'HIGH'",
            },
            Row {
                task_model: Some("\t"),
                difficulty: Some("HIGH"),
                prd_default: Some(HAIKU_MODEL),
                project_default: None,
                user_default: None,
                expected: Some(OPUS_MODEL),
                reason: "tab task + HIGH diff",
            },
            // ---- New rows for project + user defaults ----
            Row {
                task_model: None,
                difficulty: None,
                prd_default: None,
                project_default: Some(SONNET_MODEL),
                user_default: None,
                expected: Some(SONNET_MODEL),
                reason: "project default alone",
            },
            Row {
                task_model: None,
                difficulty: None,
                prd_default: None,
                project_default: None,
                user_default: Some(HAIKU_MODEL),
                expected: Some(HAIKU_MODEL),
                reason: "user default alone",
            },
            Row {
                task_model: None,
                difficulty: None,
                prd_default: Some(SONNET_MODEL),
                project_default: Some(HAIKU_MODEL),
                user_default: None,
                expected: Some(SONNET_MODEL),
                reason: "prd beats project",
            },
            Row {
                task_model: None,
                difficulty: None,
                prd_default: None,
                project_default: Some(SONNET_MODEL),
                user_default: Some(OPUS_MODEL),
                expected: Some(SONNET_MODEL),
                reason: "project beats user",
            },
            Row {
                task_model: None,
                difficulty: Some("medium"),
                prd_default: None,
                project_default: Some(HAIKU_MODEL),
                user_default: None,
                expected: Some(HAIKU_MODEL),
                reason: "medium falls past to project",
            },
            Row {
                task_model: None,
                difficulty: Some("low"),
                prd_default: None,
                project_default: None,
                user_default: Some(SONNET_MODEL),
                expected: Some(SONNET_MODEL),
                reason: "low falls past to user",
            },
            Row {
                task_model: None,
                difficulty: Some("high"),
                prd_default: None,
                project_default: Some(HAIKU_MODEL),
                user_default: Some(HAIKU_MODEL),
                expected: Some(OPUS_MODEL),
                reason: "high beats configs",
            },
            Row {
                task_model: Some(HAIKU_MODEL),
                difficulty: None,
                prd_default: None,
                project_default: Some(SONNET_MODEL),
                user_default: Some(OPUS_MODEL),
                expected: Some(HAIKU_MODEL),
                reason: "task beats configs",
            },
            Row {
                task_model: Some(""),
                difficulty: None,
                prd_default: None,
                project_default: Some("  "),
                user_default: Some(HAIKU_MODEL),
                expected: Some(HAIKU_MODEL),
                reason: "empty project falls to user",
            },
        ];

        for (i, row) in cases.iter().enumerate() {
            let result = resolve_task_model(&ModelResolutionContext {
                task_model: row.task_model,
                difficulty: row.difficulty,
                prd_default: row.prd_default,
                project_default: row.project_default,
                user_default: row.user_default,
                ..ctx()
            });
            assert_eq!(
                result,
                row.expected.map(String::from),
                "precedence row {} ({}) failed",
                i + 1,
                row.reason,
            );
        }
    }

    // ============ resolve_iteration_model tests ============

    #[test]
    fn test_resolve_iteration_model_highest_tier_wins() {
        let models = vec![
            Some(HAIKU_MODEL.to_string()),
            Some(OPUS_MODEL.to_string()),
            Some(SONNET_MODEL.to_string()),
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_iteration_model_all_none() {
        let models: Vec<Option<String>> = vec![None, None, None];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_iteration_model_empty_list() {
        let models: Vec<Option<String>> = vec![];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, None);
    }

    /// Empty touchesFiles yields a single-task cluster.
    #[test]
    fn test_resolve_iteration_model_single_task_cluster() {
        let models = vec![Some(SONNET_MODEL.to_string())];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_iteration_model_mixed_none_and_some() {
        let models = vec![None, Some(HAIKU_MODEL.to_string()), None];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_iteration_model_same_tier_returns_consistent_result() {
        let models = vec![
            Some(SONNET_MODEL.to_string()),
            Some(SONNET_MODEL.to_string()),
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_iteration_model_sonnet_beats_haiku() {
        let models = vec![
            Some(HAIKU_MODEL.to_string()),
            Some(SONNET_MODEL.to_string()),
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    /// AC: synergy cluster with 3+ overlapping tasks at different tiers.
    /// Opus in position 1 (not last) still wins.
    #[test]
    fn test_resolve_iteration_model_three_plus_tasks_opus_not_last() {
        let models = vec![
            Some(SONNET_MODEL.to_string()),
            Some(OPUS_MODEL.to_string()),
            Some(HAIKU_MODEL.to_string()),
            Some(SONNET_MODEL.to_string()),
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    /// AC: partial file overlap—some tasks have models, some are None.
    /// Only the non-None entries participate in tier selection.
    #[test]
    fn test_resolve_iteration_model_partial_overlap_with_none() {
        let models = vec![
            None,
            Some(HAIKU_MODEL.to_string()),
            None,
            Some(SONNET_MODEL.to_string()),
            None,
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(
            result,
            Some(SONNET_MODEL.to_string()),
            "sonnet should win among partial-overlap cluster"
        );
    }

    /// 3+ tasks all at the same tier—should return one of them.
    #[test]
    fn test_resolve_iteration_model_all_same_tier() {
        let models = vec![
            Some(HAIKU_MODEL.to_string()),
            Some(HAIKU_MODEL.to_string()),
            Some(HAIKU_MODEL.to_string()),
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    /// Mix of known tiers and unknown model strings.
    /// Unknown strings are ModelTier::Default (lowest), so the known tier wins.
    #[test]
    fn test_resolve_iteration_model_unknown_mixed_with_known() {
        let models = vec![
            Some("gpt-4-turbo".to_string()),
            Some(HAIKU_MODEL.to_string()),
            Some("llama-3".to_string()),
        ];
        let result = resolve_iteration_model(&models);
        assert_eq!(
            result,
            Some(HAIKU_MODEL.to_string()),
            "known tier (haiku) should beat unknown tiers"
        );
    }

    /// All entries are unknown model strings—one of them should be returned.
    #[test]
    fn test_resolve_iteration_model_all_unknown() {
        let models = vec![Some("gpt-4".to_string()), Some("llama-3".to_string())];
        let result = resolve_iteration_model(&models);
        // All are ModelTier::Default; max_by_key picks the last tied element
        assert!(
            result.is_some(),
            "should return some model even if all are unknown tier"
        );
    }

    // ============ escalate_model tests ============

    #[test]
    fn test_escalate_model_haiku_to_sonnet() {
        let result = escalate_model(Some(HAIKU_MODEL));
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_model_sonnet_to_opus() {
        let result = escalate_model(Some(SONNET_MODEL));
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_model_opus_stays_opus() {
        let result = escalate_model(Some(OPUS_MODEL));
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_escalate_model_none_stays_none() {
        let result = escalate_model(None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_escalate_model_unknown_returns_none() {
        let result = escalate_model(Some("gpt-4"));
        assert_eq!(result, None, "unknown model tier cannot be escalated");
    }

    /// AC: unknown model string doesn't crash—returns None, not panic.
    #[test]
    fn test_escalate_model_various_unknown_strings_no_crash() {
        assert_eq!(escalate_model(Some("")), None);
        assert_eq!(escalate_model(Some("  ")), None);
        assert_eq!(escalate_model(Some("llama-3-70b")), None);
        assert_eq!(escalate_model(Some("gemini-pro")), None);
        assert_eq!(
            escalate_model(Some("totally-unknown-model-xyz")),
            None,
            "arbitrary unknown string should safely return None"
        );
    }

    /// Escalate with case-variant model strings.
    #[test]
    fn test_escalate_model_case_variants() {
        assert_eq!(
            escalate_model(Some("HAIKU")),
            Some(SONNET_MODEL.to_string()),
            "uppercase HAIKU should escalate to sonnet"
        );
        assert_eq!(
            escalate_model(Some("Sonnet")),
            Some(OPUS_MODEL.to_string()),
            "mixed-case Sonnet should escalate to opus"
        );
        assert_eq!(
            escalate_model(Some("OPUS")),
            Some(OPUS_MODEL.to_string()),
            "uppercase OPUS should stay at opus ceiling"
        );
    }

    /// Double escalation chain: haiku → sonnet → opus.
    #[test]
    fn test_escalate_model_double_escalation_chain() {
        let first = escalate_model(Some(HAIKU_MODEL));
        assert_eq!(first, Some(SONNET_MODEL.to_string()));

        let second = escalate_model(first.as_deref());
        assert_eq!(second, Some(OPUS_MODEL.to_string()));

        let third = escalate_model(second.as_deref());
        assert_eq!(
            third,
            Some(OPUS_MODEL.to_string()),
            "opus is the ceiling; further escalation stays at opus"
        );
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
        assert!(is_1m_model(Some(OPUS_MODEL_1M)));
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

    #[test]
    fn test_to_1m_model_opus_returns_1m() {
        assert_eq!(to_1m_model(Some(OPUS_MODEL)), Some(OPUS_MODEL_1M));
    }

    #[test]
    fn test_to_1m_model_already_1m_returns_none() {
        assert_eq!(to_1m_model(Some(OPUS_MODEL_1M)), None);
    }

    #[test]
    fn test_to_1m_model_non_opus_returns_none() {
        assert_eq!(to_1m_model(Some(SONNET_MODEL)), None);
        assert_eq!(to_1m_model(Some(HAIKU_MODEL)), None);
    }

    #[test]
    fn test_to_1m_model_none_returns_none() {
        assert_eq!(to_1m_model(None), None);
    }

    #[test]
    fn test_to_1m_model_unknown_returns_none() {
        assert_eq!(to_1m_model(Some("gpt-4")), None);
        assert_eq!(to_1m_model(Some("")), None);
    }

    #[test]
    fn test_to_1m_model_case_variant_opus() {
        assert_eq!(
            to_1m_model(Some("OPUS")),
            Some(OPUS_MODEL_1M),
            "case-insensitive opus detection should return 1M variant"
        );
    }

    #[test]
    fn test_1m_model_tier_is_opus() {
        assert_eq!(
            model_tier(Some(OPUS_MODEL_1M)),
            ModelTier::Opus,
            "1M model should still be classified as Opus tier"
        );
    }

    // ============ is_review_class tests ============

    #[test]
    fn test_is_review_class_positive_unprefixed() {
        assert!(is_review_class("CODE-REVIEW-1"));
        assert!(is_review_class("MILESTONE-FINAL"));
        assert!(is_review_class("REVIEW-001"));
        assert!(is_review_class("CODE-REVIEW-007"));
        assert!(is_review_class("REVIEW-FINAL"));
    }

    #[test]
    fn test_is_review_class_positive_prefixed() {
        // Regression guard: bare starts_with("CODE-REVIEW-") would fail this.
        assert!(
            is_review_class("8d71d1f7-CODE-REVIEW-1"),
            "prefixed CODE-REVIEW-1 must be true"
        );
        assert!(is_review_class("8d71d1f7-MILESTONE-FINAL"));
        assert!(is_review_class("8d71d1f7-REVIEW-001"));
    }

    #[test]
    fn test_is_review_class_negative_unprefixed() {
        assert!(!is_review_class("REFACTOR-REVIEW-FINAL"));
        assert!(!is_review_class("MILESTONE-1"));
        assert!(!is_review_class("MILESTONE-2"));
        assert!(!is_review_class("VERIFY-001"));
        assert!(!is_review_class("REFACTOR-001"));
        assert!(!is_review_class("FEAT-001"));
    }

    #[test]
    fn test_is_review_class_negative_prefixed() {
        // Stripping first '-' (wrong) would turn REFACTOR-REVIEW-FINAL into
        // REVIEW-FINAL and falsely match REVIEW-; strip only 8-hex-char prefix.
        assert!(
            !is_review_class("8d71d1f7-REFACTOR-REVIEW-FINAL"),
            "prefixed REFACTOR-REVIEW-FINAL must be false"
        );
        assert!(!is_review_class("8d71d1f7-CODE-FIX-001"));
        assert!(!is_review_class("8d71d1f7-FEAT-001"));
        assert!(!is_review_class("8d71d1f7-MILESTONE-1"));
    }

    #[test]
    fn test_is_review_class_non_hex_prefix_not_stripped() {
        // "CODE-REV" starts with non-hex chars; must NOT be treated as a prefix.
        // id = "CODE-REV-CODE-REVIEW-1": body stays as-is because first 8 chars
        // include uppercase which is outside [0-9a-f].
        assert!(!is_review_class("CODE-REV-CODE-REVIEW-1"));
    }

    #[test]
    fn test_is_review_class_uppercase_hex_prefix_not_stripped() {
        // Uppercase hex like "ABCDEF01" is outside [0-9a-f]; must not strip.
        assert!(!is_review_class("ABCDEF01-CODE-REVIEW-1"));
    }

    // ============ escalate_below_opus tests ============

    #[test]
    fn test_escalate_below_opus_haiku_to_sonnet() {
        assert_eq!(escalate_below_opus(Some(HAIKU_MODEL)), Some(SONNET_MODEL));
    }

    #[test]
    fn test_escalate_below_opus_sonnet_to_opus() {
        assert_eq!(escalate_below_opus(Some(SONNET_MODEL)), Some(OPUS_MODEL));
    }

    #[test]
    fn test_escalate_below_opus_opus_is_ceiling() {
        assert_eq!(
            escalate_below_opus(Some(OPUS_MODEL)),
            None,
            "opus is the ceiling for this rung; further escalation handled by to_1m_model"
        );
    }

    #[test]
    fn test_escalate_below_opus_opus_1m_is_ceiling() {
        assert_eq!(
            escalate_below_opus(Some(OPUS_MODEL_1M)),
            None,
            "1M variant counts as Opus tier — no further escalation"
        );
    }

    #[test]
    fn test_escalate_below_opus_unknown_returns_none() {
        assert_eq!(escalate_below_opus(Some("gpt-4")), None);
        assert_eq!(escalate_below_opus(Some("")), None);
    }

    #[test]
    fn test_escalate_below_opus_none_returns_none() {
        assert_eq!(escalate_below_opus(None), None);
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
}
