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
/// 1. `task_model` — explicit model set on the task (empty/whitespace-only normalized to `None`)
/// 2. `difficulty == "high"` (case-insensitive) → `OPUS_MODEL`
/// 3. `prd_default` — default model from the PRD metadata
/// 4. `None` — no model preference
pub fn resolve_task_model(
    task_model: Option<&str>,
    difficulty: Option<&str>,
    prd_default: Option<&str>,
) -> Option<String> {
    if let Some(m) = task_model.filter(|m| !m.trim().is_empty()) {
        return Some(m.to_string());
    }

    if difficulty.is_some_and(|d| d.eq_ignore_ascii_case("high")) {
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

    /// AC: empty string task_model normalizes to None, falling through to difficulty/prd_default.
    #[test]
    fn test_resolve_task_model_empty_string_falls_through() {
        let result = resolve_task_model(Some(""), Some("high"), Some(SONNET_MODEL));
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "empty string task_model should normalize to None, falling through to high difficulty"
        );
    }

    #[test]
    fn test_resolve_task_model_empty_string_without_fallbacks() {
        let result = resolve_task_model(Some(""), None, None);
        assert_eq!(
            result, None,
            "empty string task_model should normalize to None with no fallbacks"
        );
    }

    #[test]
    fn test_resolve_task_model_unknown_difficulty_falls_through() {
        let result = resolve_task_model(None, Some("critical"), None);
        assert_eq!(
            result, None,
            "unrecognized difficulty should not trigger any escalation"
        );
    }

    #[test]
    fn test_resolve_task_model_unknown_difficulty_with_prd_default() {
        let result = resolve_task_model(None, Some("critical"), Some(HAIKU_MODEL));
        assert_eq!(
            result,
            Some(HAIKU_MODEL.to_string()),
            "unrecognized difficulty should fall through to prd_default"
        );
    }

    #[test]
    fn test_resolve_task_model_empty_difficulty_falls_through() {
        let result = resolve_task_model(None, Some(""), Some(SONNET_MODEL));
        assert_eq!(
            result,
            Some(SONNET_MODEL.to_string()),
            "empty difficulty string should fall through to prd_default"
        );
    }

    // ============ Parameterized precedence table ============

    /// Exhaustive precedence combinations: task_model × difficulty × prd_default.
    /// Each row tests one combination to verify the full precedence chain.
    #[test]
    fn test_resolve_task_model_precedence_table() {
        // (task_model, difficulty, prd_default) → expected
        let cases: Vec<(Option<&str>, Option<&str>, Option<&str>, Option<&str>)> = vec![
            // Row 1: all None
            (None, None, None, None),
            // Row 2: only prd_default set
            (None, None, Some(SONNET_MODEL), Some(SONNET_MODEL)),
            // Row 3: high difficulty, no prd_default
            (None, Some("high"), None, Some(OPUS_MODEL)),
            // Row 4: high difficulty beats prd_default
            (None, Some("high"), Some(SONNET_MODEL), Some(OPUS_MODEL)),
            // Row 5: medium difficulty, no fallback
            (None, Some("medium"), None, None),
            // Row 6: medium difficulty, falls through to prd_default
            (None, Some("medium"), Some(HAIKU_MODEL), Some(HAIKU_MODEL)),
            // Row 7: low difficulty, no fallback
            (None, Some("low"), None, None),
            // Row 8: low difficulty, falls through to prd_default
            (None, Some("low"), Some(SONNET_MODEL), Some(SONNET_MODEL)),
            // Row 9: task_model alone
            (Some(HAIKU_MODEL), None, None, Some(HAIKU_MODEL)),
            // Row 10: task_model beats prd_default
            (Some(HAIKU_MODEL), None, Some(OPUS_MODEL), Some(HAIKU_MODEL)),
            // Row 11: task_model beats high difficulty
            (Some(HAIKU_MODEL), Some("high"), None, Some(HAIKU_MODEL)),
            // Row 12: task_model wins over everything
            (
                Some(HAIKU_MODEL),
                Some("high"),
                Some(OPUS_MODEL),
                Some(HAIKU_MODEL),
            ),
            // Row 13: task_model wins with medium difficulty
            (
                Some(SONNET_MODEL),
                Some("medium"),
                Some(OPUS_MODEL),
                Some(SONNET_MODEL),
            ),
            // Row 14: task_model wins with low difficulty
            (
                Some(OPUS_MODEL),
                Some("low"),
                Some(HAIKU_MODEL),
                Some(OPUS_MODEL),
            ),
            // Row 15: empty-string task_model falls through to high difficulty
            (Some(""), Some("high"), None, Some(OPUS_MODEL)),
            // Row 16: whitespace-only task_model falls through to prd_default
            (Some("  "), None, Some(SONNET_MODEL), Some(SONNET_MODEL)),
            // Row 17: empty-string task_model with no fallbacks returns None
            (Some(""), None, None, None),
            // Row 18: case-variant "High" triggers opus escalation
            (None, Some("High"), None, Some(OPUS_MODEL)),
            // Row 19: case-variant "HIGH" triggers opus escalation
            (None, Some("HIGH"), None, Some(OPUS_MODEL)),
            // Row 20: case-variant "hIgH" triggers opus escalation
            (None, Some("hIgH"), None, Some(OPUS_MODEL)),
            // Row 21: whitespace-only task_model with "HIGH" difficulty
            (
                Some("\t"),
                Some("HIGH"),
                Some(HAIKU_MODEL),
                Some(OPUS_MODEL),
            ),
        ];

        for (i, (task_model, difficulty, prd_default, expected)) in cases.iter().enumerate() {
            let result = resolve_task_model(*task_model, *difficulty, *prd_default);
            assert_eq!(
                result,
                expected.map(String::from),
                "precedence row {} failed: task={:?}, diff={:?}, prd={:?}",
                i + 1,
                task_model,
                difficulty,
                prd_default,
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
}
