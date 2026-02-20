//! Model selection and resolution logic for the loop engine.
//!
//! Pure functions for determining which Claude model to use for task execution.
//! No I/O dependencies — all inputs are passed as parameters.

/// Well-known model identifiers.
pub const OPUS_MODEL: &str = "claude-opus-4-6";
pub const SONNET_MODEL: &str = "claude-sonnet-4-6";
pub const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";

/// Model tier ordering for comparison.
///
/// Variants are ordered from lowest to highest capability/cost.
/// `Default` represents an unknown or unspecified model.
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

/// Resolve which model a single task should use.
///
/// Precedence (highest to lowest):
/// 1. `task_model` — explicit model set on the task
/// 2. `difficulty == "high"` → `OPUS_MODEL`
/// 3. `prd_default` — default model from the PRD metadata
/// 4. `None` — no model preference
pub fn resolve_task_model(
    task_model: Option<&str>,
    difficulty: Option<&str>,
    prd_default: Option<&str>,
) -> Option<String> {
    if let Some(m) = task_model {
        return Some(m.to_string());
    }

    if difficulty == Some("high") {
        return Some(OPUS_MODEL.to_string());
    }

    if let Some(d) = prd_default {
        return Some(d.to_string());
    }

    None
}

/// Resolve the model for an iteration by selecting the highest-tier model
/// from a synergy cluster of tasks.
///
/// When multiple tasks run in the same iteration (synergy cluster),
/// the highest-tier model wins so that all tasks get adequate capability.
///
/// Returns `None` if the slice is empty or all entries are `None`.
pub fn resolve_iteration_model(task_models: &[Option<String>]) -> Option<String> {
    task_models
        .iter()
        .filter_map(|m| m.as_deref())
        .max_by_key(|m| model_tier(Some(m)))
        .map(String::from)
}

/// Escalate a model to the next higher tier.
///
/// - haiku → sonnet
/// - sonnet → opus
/// - opus → opus (already at ceiling)
/// - None → None
/// - unknown/Default tier → None (cannot escalate unrecognized model)
pub fn escalate_model(model: Option<&str>) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ============ model_tier tests ============

    #[test]
    fn test_model_tier_opus_constants_and_substrings() {
        assert_eq!(model_tier(Some(OPUS_MODEL)), ModelTier::Opus);
        assert_eq!(model_tier(Some("claude-opus-4-6")), ModelTier::Opus);
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

    #[test]
    fn test_model_tier_ordering() {
        assert!(ModelTier::Opus > ModelTier::Sonnet);
        assert!(ModelTier::Sonnet > ModelTier::Haiku);
        assert!(ModelTier::Haiku > ModelTier::Default);
    }

    // ============ resolve_task_model tests ============

    #[test]
    fn test_resolve_task_model_task_model_wins_over_everything() {
        let result = resolve_task_model(Some(HAIKU_MODEL), Some("high"), Some(SONNET_MODEL));
        assert_eq!(result, Some(HAIKU_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_task_model_overrides_difficulty() {
        let result = resolve_task_model(Some(SONNET_MODEL), Some("high"), None);
        assert_eq!(
            result,
            Some(SONNET_MODEL.to_string()),
            "explicit task model must override high difficulty"
        );
    }

    #[test]
    fn test_resolve_task_model_high_difficulty_forces_opus() {
        let result = resolve_task_model(None, Some("high"), Some(SONNET_MODEL));
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_high_difficulty_without_prd_default() {
        let result = resolve_task_model(None, Some("high"), None);
        assert_eq!(result, Some(OPUS_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_prd_default_fallback() {
        let result = resolve_task_model(None, None, Some(SONNET_MODEL));
        assert_eq!(result, Some(SONNET_MODEL.to_string()));
    }

    #[test]
    fn test_resolve_task_model_all_none_returns_none() {
        let result = resolve_task_model(None, None, None);
        assert_eq!(result, None);
    }

    /// Known-bad discriminator: "medium" difficulty must NOT escalate to opus.
    /// A naive implementation that treats any difficulty as "force opus" would fail this.
    #[test]
    fn test_resolve_task_model_medium_difficulty_does_not_escalate() {
        let result = resolve_task_model(None, Some("medium"), None);
        assert_eq!(result, None, "medium difficulty must NOT escalate to opus");
    }

    #[test]
    fn test_resolve_task_model_low_difficulty_falls_through_to_prd_default() {
        let result = resolve_task_model(None, Some("low"), Some(HAIKU_MODEL));
        assert_eq!(
            result,
            Some(HAIKU_MODEL.to_string()),
            "low difficulty should fall through to prd_default"
        );
    }

    #[test]
    fn test_resolve_task_model_medium_difficulty_with_prd_default() {
        let result = resolve_task_model(None, Some("medium"), Some(HAIKU_MODEL));
        assert_eq!(
            result,
            Some(HAIKU_MODEL.to_string()),
            "medium difficulty should fall through to prd_default, not force opus"
        );
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
}
