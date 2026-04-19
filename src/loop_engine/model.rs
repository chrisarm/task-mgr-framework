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

/// Well-known model identifiers.
pub const OPUS_MODEL: &str = "claude-opus-4-7";
pub const OPUS_MODEL_1M: &str = "claude-opus-4-7[1m]";
pub const SONNET_MODEL: &str = "claude-sonnet-4-6";
pub const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";

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
}

/// Resolve which model a single task should use.
///
/// Precedence (highest to lowest):
/// 1. `task_model` — explicit model set on the task
/// 2. `difficulty == "high"` (case-insensitive) → `OPUS_MODEL`
/// 3. `prd_default` — default from PRD metadata
/// 4. `project_default` — default from `.task-mgr/config.json`
/// 5. `user_default` — default from `$XDG_CONFIG_HOME/task-mgr/config.json`
/// 6. `None` — no preference
///
/// Empty / whitespace-only strings in any field are normalized to `None` so
/// missing config values don't override real ones.
pub fn resolve_task_model(ctx: &ModelResolutionContext<'_>) -> Option<String> {
    if let Some(m) = normalize(ctx.task_model) {
        return Some(m.to_string());
    }
    if ctx
        .difficulty
        .is_some_and(|d| d.eq_ignore_ascii_case("high"))
    {
        return Some(OPUS_MODEL.to_string());
    }
    for fallback in [ctx.prd_default, ctx.project_default, ctx.user_default] {
        if let Some(m) = normalize(fallback) {
            return Some(m.to_string());
        }
    }
    None
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

/// Check whether a model string is already a 1M-context variant (contains `[1m]`).
pub fn is_1m_model(model: Option<&str>) -> bool {
    model.is_some_and(|m| m.to_lowercase().contains("[1m]"))
}

/// Return the 1M-context variant for a model, if one exists.
///
/// Currently only Opus has a 1M variant. Returns `None` if the model is
/// already 1M, not Opus-tier, or `None`.
pub fn to_1m_model(model: Option<&str>) -> Option<&'static str> {
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
}
