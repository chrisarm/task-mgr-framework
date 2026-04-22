//! Pure statistical math functions for calibration weight computation.
//!
//! Contains the arithmetic primitives used by `calibrate.rs`: correlation
//! computation, weight adjustment, and weight clamping. All functions are
//! stateless and take no database connections.

use crate::loop_engine::calibrate::TaskOutcome;

/// Lower bound multiplier for weight adjustment (0.5x of default).
const WEIGHT_LOWER_BOUND: f64 = 0.5;

/// Upper bound multiplier for weight adjustment (2.0x of default).
const WEIGHT_UPPER_BOUND: f64 = 2.0;

/// Clamp a positive weight to [0.5x, 2.0x] of its default.
pub(crate) fn clamp_weight(value: i32, default: i32) -> i32 {
    let lower = (default as f64 * WEIGHT_LOWER_BOUND) as i32;
    let upper = (default as f64 * WEIGHT_UPPER_BOUND) as i32;
    value.clamp(lower, upper)
}

/// Clamp a negative weight to [2.0x, 0.5x] of its default (inverted bounds).
pub(crate) fn clamp_negative_weight(value: i32, default: i32) -> i32 {
    // For negative weights like CONFLICT_PENALTY=-5:
    // lower bound (more negative) = default * 2.0 = -10
    // upper bound (less negative) = default * 0.5 = -2 (rounded)
    let lower = (default as f64 * WEIGHT_UPPER_BOUND) as i32; // more negative
    let upper = (default as f64 * WEIGHT_LOWER_BOUND) as i32; // less negative
    value.clamp(lower, upper)
}

/// Compute point-biserial correlation between a dimension and success.
///
/// Returns a value in [-1, 1]:
/// - Positive: higher dimension values correlate with first-try success
/// - Negative: higher dimension values correlate with failure
/// - Zero: no correlation (or insufficient data for one group)
pub(crate) fn compute_correlation<F>(outcomes: &[TaskOutcome], dimension: F) -> f64
where
    F: Fn(&TaskOutcome) -> f64,
{
    let (mut success_sum, mut failure_sum) = (0.0_f64, 0.0_f64);
    let (mut success_count, mut failure_count) = (0_usize, 0_usize);

    for o in outcomes {
        let val = dimension(o);
        if o.first_try_success {
            success_sum += val;
            success_count += 1;
        } else {
            failure_sum += val;
            failure_count += 1;
        }
    }

    // Need both groups to compute correlation
    if success_count == 0 || failure_count == 0 {
        return 0.0;
    }

    let success_mean = success_sum / success_count as f64;
    let failure_mean = failure_sum / failure_count as f64;

    // Normalized difference: (success_mean - failure_mean) / max(|success_mean|, |failure_mean|, 1.0)
    // This gives a value roughly in [-1, 1] without requiring full standard deviation calculation
    let normalizer = success_mean.abs().max(failure_mean.abs()).max(1.0);
    let correlation = (success_mean - failure_mean) / normalizer;

    // Clamp to [-1, 1] for safety
    correlation.clamp(-1.0, 1.0)
}

/// Adjust a weight based on correlation value.
///
/// weight = default * (1 + correlation * factor)
/// For positive default weights, positive correlation increases the weight.
/// For negative default weights, the math still works correctly.
pub(crate) fn adjust_weight(default: i32, correlation: f64, factor: f64) -> i32 {
    let adjusted = default as f64 * (1.0 + correlation * factor);
    adjusted.round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::calibrate::TaskOutcome;

    // --- clamp_weight tests ---

    #[test]
    fn test_clamp_positive_weight_upper_bound() {
        // FILE_OVERLAP_SCORE=10, upper=20
        let clamped = clamp_weight(50, 10);
        assert_eq!(clamped, 20, "Should clamp to 2.0x of default");
    }

    #[test]
    fn test_clamp_positive_weight_lower_bound() {
        // FILE_OVERLAP_SCORE=10, lower=5
        let clamped = clamp_weight(1, 10);
        assert_eq!(clamped, 5, "Should clamp to 0.5x of default");
    }

    #[test]
    fn test_clamp_positive_weight_within_bounds() {
        let clamped = clamp_weight(15, 10);
        assert_eq!(clamped, 15, "Should not clamp value within bounds");
    }

    #[test]
    fn test_clamp_at_exact_lower_bound() {
        // FILE_OVERLAP_SCORE=10, lower=5
        let clamped = clamp_weight(5, 10);
        assert_eq!(
            clamped, 5,
            "Value exactly at lower bound should be unchanged"
        );
    }

    #[test]
    fn test_clamp_at_exact_upper_bound() {
        // FILE_OVERLAP_SCORE=10, upper=20
        let clamped = clamp_weight(20, 10);
        assert_eq!(
            clamped, 20,
            "Value exactly at upper bound should be unchanged"
        );
    }

    #[test]
    fn test_clamp_weight_zero_default() {
        // Edge case: if default is 0, bounds are 0..0
        let clamped = clamp_weight(5, 0);
        assert_eq!(clamped, 0, "Zero default should clamp everything to 0");
    }

    // --- clamp_negative_weight tests ---

    #[test]
    fn test_clamp_negative_weight_more_negative() {
        // CONFLICT_PENALTY=-5, lower (more negative)=-10
        let clamped = clamp_negative_weight(-15, -5);
        assert_eq!(
            clamped, -10,
            "Should clamp to 2.0x of default (more negative)"
        );
    }

    #[test]
    fn test_clamp_negative_weight_less_negative() {
        // CONFLICT_PENALTY=-5, upper (less negative)=-2
        let clamped = clamp_negative_weight(-1, -5);
        assert_eq!(
            clamped, -2,
            "Should clamp to 0.5x of default (less negative)"
        );
    }

    #[test]
    fn test_clamp_negative_weight_within_bounds() {
        let clamped = clamp_negative_weight(-7, -5);
        assert_eq!(clamped, -7, "Should not clamp value within bounds");
    }

    #[test]
    fn test_clamp_negative_at_exact_lower_bound() {
        // CONFLICT_PENALTY=-5, lower (more negative)=-10
        let clamped = clamp_negative_weight(-10, -5);
        assert_eq!(
            clamped, -10,
            "Negative value exactly at lower bound should be unchanged"
        );
    }

    #[test]
    fn test_clamp_negative_at_exact_upper_bound() {
        // CONFLICT_PENALTY=-5, upper (less negative)=-2
        let clamped = clamp_negative_weight(-2, -5);
        assert_eq!(
            clamped, -2,
            "Negative value exactly at upper bound should be unchanged"
        );
    }

    // --- compute_correlation tests ---

    #[test]
    fn test_compute_correlation_positive() {
        // Success tasks have higher file counts → positive correlation
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 5.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 4.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 1.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];

        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert!(corr > 0.0, "Should be positive correlation: {}", corr);
    }

    #[test]
    fn test_compute_correlation_negative() {
        // Success tasks have lower file counts → negative correlation
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 5.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];

        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert!(corr < 0.0, "Should be negative correlation: {}", corr);
    }

    #[test]
    fn test_compute_correlation_all_same_group() {
        // All successes, no failures → zero correlation
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 3.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 5.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];

        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert_eq!(corr, 0.0, "All same group should return 0");
    }

    #[test]
    fn test_compute_correlation_empty() {
        let outcomes: Vec<TaskOutcome> = vec![];
        // Empty outcomes won't reach compute_correlation in production,
        // but test the function directly for safety
        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert_eq!(corr, 0.0, "Empty should return 0");
    }

    #[test]
    fn test_correlation_clamped_to_minus_one() {
        // All success = 0, all failure = very high → correlation should be <= -1 → clamped to -1
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 1000.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];
        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert!(
            (-1.0..=0.0).contains(&corr),
            "Extreme negative correlation should be clamped: {}",
            corr
        );
    }

    #[test]
    fn test_correlation_clamped_to_plus_one() {
        // All success = very high, all failure = 0 → correlation should be >= 1 → clamped to 1
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 1000.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];
        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert!(
            (0.0..=1.0).contains(&corr),
            "Extreme positive correlation should be clamped: {}",
            corr
        );
    }

    #[test]
    fn test_correlation_equal_means_zero() {
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 3.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 3.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];
        let corr = compute_correlation(&outcomes, |o| o.file_overlap_count);
        assert_eq!(corr, 0.0, "Equal means should produce zero correlation");
    }

    #[test]
    fn test_correlation_synergy_dimension() {
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 0.0,
                synergy_count: 10.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 0.0,
                synergy_count: 8.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 0.0,
                synergy_count: 1.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
        ];
        let corr = compute_correlation(&outcomes, |o| o.synergy_count);
        assert!(
            corr > 0.0,
            "High synergy success should give positive correlation: {}",
            corr
        );
    }

    #[test]
    fn test_correlation_conflict_dimension() {
        // Successful tasks have low conflict, failed have high → negative correlation for conflict
        let outcomes = vec![
            TaskOutcome {
                first_try_success: true,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 0.0,
            },
            TaskOutcome {
                first_try_success: false,
                file_overlap_count: 0.0,
                synergy_count: 0.0,
                conflict_count: 5.0,
            },
        ];
        let corr = compute_correlation(&outcomes, |o| o.conflict_count);
        assert!(
            corr < 0.0,
            "High conflict in failures should give negative correlation: {}",
            corr
        );
    }

    // --- adjust_weight tests ---

    #[test]
    fn test_adjust_weight_positive_correlation() {
        // default=10, correlation=0.5, factor=0.5 → 10 * (1 + 0.25) = 12.5 → 13
        let result = adjust_weight(10, 0.5, 0.5);
        assert_eq!(result, 13);
    }

    #[test]
    fn test_adjust_weight_negative_correlation() {
        // default=10, correlation=-0.5, factor=0.5 → 10 * (1 - 0.25) = 7.5 → 8
        let result = adjust_weight(10, -0.5, 0.5);
        assert_eq!(result, 8);
    }

    #[test]
    fn test_adjust_weight_zero_correlation() {
        // default=10, correlation=0, factor=0.5 → 10 * 1 = 10
        let result = adjust_weight(10, 0.0, 0.5);
        assert_eq!(result, 10);
    }

    #[test]
    fn test_adjust_weight_negative_default() {
        // default=-5, correlation=0.5, factor=0.5 → -5 * (1 + 0.25) = -6.25 → -6
        let result = adjust_weight(-5, 0.5, 0.5);
        assert_eq!(result, -6);
    }

    #[test]
    fn test_adjust_weight_max_positive_correlation() {
        // default=10, correlation=1.0, factor=0.5 → 10 * (1 + 0.5) = 15
        let result = adjust_weight(10, 1.0, 0.5);
        assert_eq!(result, 15);
    }

    #[test]
    fn test_adjust_weight_max_negative_correlation() {
        // default=10, correlation=-1.0, factor=0.5 → 10 * (1 - 0.5) = 5
        let result = adjust_weight(10, -1.0, 0.5);
        assert_eq!(result, 5);
    }
}
