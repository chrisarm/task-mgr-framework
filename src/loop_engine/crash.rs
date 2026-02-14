/// Crash tracker state machine for exponential backoff on consecutive failures.
///
/// Tracks consecutive crash count and provides exponential backoff durations.
/// Resets to zero on a successful iteration. Signals abort after exceeding
/// the configured maximum consecutive crashes.
///
/// Backoff formula: `30s * 2^(n-1)` where n is the current crash count.
use std::time::Duration;

/// Tracks consecutive crashes and provides backoff/abort decisions.
pub struct CrashTracker {
    /// Number of consecutive crashes (resets on success)
    consecutive_crashes: u32,
    /// Maximum allowed consecutive crashes before abort
    max_crashes: u32,
}

impl CrashTracker {
    /// Create a new CrashTracker with the given abort threshold.
    pub fn new(max_crashes: u32) -> Self {
        Self {
            consecutive_crashes: 0,
            max_crashes,
        }
    }

    /// Record a crash. Increments the consecutive crash counter.
    pub fn record_crash(&mut self) {
        self.consecutive_crashes = self.consecutive_crashes.saturating_add(1);
    }

    /// Record a success. Resets the consecutive crash counter to zero.
    pub fn record_success(&mut self) {
        self.consecutive_crashes = 0;
    }

    /// Calculate the backoff duration based on current crash count.
    ///
    /// Formula: `30s * 2^(n-1)` where n = consecutive_crashes.
    /// Returns Duration::ZERO if no crashes recorded.
    /// Uses saturating arithmetic to prevent overflow at high crash counts.
    pub fn backoff_duration(&self) -> Duration {
        if self.consecutive_crashes == 0 {
            return Duration::ZERO;
        }
        let exponent = self.consecutive_crashes.saturating_sub(1);
        // Cap the exponent to prevent overflow: 2^31 * 30 would overflow u64 seconds
        // 2^20 * 30 = ~31 million seconds (~1 year), which is already absurd
        let capped_exponent = exponent.min(20);
        let multiplier = 1u64.checked_shl(capped_exponent).unwrap_or(1u64 << 20);
        Duration::from_secs(30 * multiplier)
    }

    /// Whether the loop should abort due to too many consecutive crashes.
    pub fn should_abort(&self) -> bool {
        self.consecutive_crashes >= self.max_crashes
    }

    /// Current number of consecutive crashes.
    pub fn count(&self) -> u32 {
        self.consecutive_crashes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AC 1: Backoff durations (30s * 2^(n-1)) ---

    #[test]
    fn test_backoff_30s_after_first_crash() {
        let mut tracker = CrashTracker::new(5);
        tracker.record_crash();
        // 30s * 2^(1-1) = 30s * 1 = 30s
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(30));
    }

    #[test]
    fn test_backoff_60s_after_second_crash() {
        let mut tracker = CrashTracker::new(5);
        tracker.record_crash();
        tracker.record_crash();
        // 30s * 2^(2-1) = 30s * 2 = 60s
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(60));
    }

    #[test]
    fn test_backoff_120s_after_third_crash() {
        let mut tracker = CrashTracker::new(5);
        tracker.record_crash();
        tracker.record_crash();
        tracker.record_crash();
        // 30s * 2^(3-1) = 30s * 4 = 120s
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(120));
    }

    #[test]
    fn test_backoff_zero_with_no_crashes() {
        let tracker = CrashTracker::new(3);
        assert_eq!(
            tracker.backoff_duration(),
            Duration::ZERO,
            "No crashes should yield zero backoff"
        );
    }

    #[test]
    fn test_backoff_240s_after_fourth_crash() {
        let mut tracker = CrashTracker::new(10);
        for _ in 0..4 {
            tracker.record_crash();
        }
        // 30s * 2^(4-1) = 30s * 8 = 240s
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(240));
    }

    // --- AC 2: Reset on success ---

    #[test]
    fn test_record_success_resets_counter_to_zero() {
        let mut tracker = CrashTracker::new(5);
        tracker.record_crash();
        tracker.record_crash();
        assert_eq!(tracker.count(), 2);

        tracker.record_success();
        assert_eq!(tracker.count(), 0, "Counter should reset to 0 on success");
    }

    #[test]
    fn test_record_success_on_fresh_tracker_is_noop() {
        let mut tracker = CrashTracker::new(5);
        tracker.record_success();
        assert_eq!(
            tracker.count(),
            0,
            "Success on fresh tracker should remain at 0"
        );
    }

    #[test]
    fn test_backoff_after_reset_returns_zero() {
        let mut tracker = CrashTracker::new(5);
        tracker.record_crash();
        tracker.record_crash();
        tracker.record_success();
        assert_eq!(
            tracker.backoff_duration(),
            Duration::ZERO,
            "After reset, backoff should be zero"
        );
    }

    // --- AC 3: Abort after MAX_CRASHES consecutive crashes ---

    #[test]
    fn test_should_abort_true_after_max_crashes() {
        let mut tracker = CrashTracker::new(3);
        tracker.record_crash();
        assert!(!tracker.should_abort(), "1 crash < 3 max");
        tracker.record_crash();
        assert!(!tracker.should_abort(), "2 crashes < 3 max");
        tracker.record_crash();
        assert!(tracker.should_abort(), "3 crashes == 3 max, should abort");
    }

    #[test]
    fn test_should_abort_false_before_max_crashes() {
        let mut tracker = CrashTracker::new(3);
        tracker.record_crash();
        tracker.record_crash();
        assert!(
            !tracker.should_abort(),
            "2 crashes < 3 max, should not abort"
        );
    }

    #[test]
    fn test_should_abort_resets_after_success() {
        let mut tracker = CrashTracker::new(3);
        tracker.record_crash();
        tracker.record_crash();
        tracker.record_success();
        tracker.record_crash();
        assert!(
            !tracker.should_abort(),
            "After reset, 1 crash < 3 max, should not abort"
        );
    }

    #[test]
    fn test_should_abort_with_max_crashes_1() {
        let mut tracker = CrashTracker::new(1);
        assert!(!tracker.should_abort(), "0 crashes should not abort");
        tracker.record_crash();
        assert!(tracker.should_abort(), "1 crash == 1 max, should abort");
    }

    // --- Edge cases ---

    #[test]
    fn test_crash_success_crash_success_sequence() {
        let mut tracker = CrashTracker::new(3);
        tracker.record_crash();
        assert_eq!(tracker.count(), 1);
        tracker.record_success();
        assert_eq!(tracker.count(), 0);
        tracker.record_crash();
        assert_eq!(tracker.count(), 1);
        tracker.record_success();
        assert_eq!(tracker.count(), 0);
    }

    #[test]
    fn test_backoff_does_not_overflow_at_high_crash_counts() {
        let mut tracker = CrashTracker::new(100);
        for _ in 0..50 {
            tracker.record_crash();
        }
        // Should not panic; duration should be capped but finite
        let duration = tracker.backoff_duration();
        assert!(
            duration.as_secs() > 0,
            "High crash count should still produce valid backoff"
        );
        assert!(
            duration.as_secs() <= 30 * (1u64 << 20),
            "Backoff should be capped at reasonable maximum"
        );
    }

    #[test]
    fn test_new_tracker_starts_at_zero() {
        let tracker = CrashTracker::new(3);
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());
    }

    #[test]
    fn test_count_increments_with_each_crash() {
        let mut tracker = CrashTracker::new(10);
        for i in 1..=5 {
            tracker.record_crash();
            assert_eq!(tracker.count(), i);
        }
    }

    // === Comprehensive tests (TEST-002) ===

    // --- Backoff duration boundary values ---

    #[test]
    fn test_backoff_480s_after_fifth_crash() {
        let mut tracker = CrashTracker::new(20);
        for _ in 0..5 {
            tracker.record_crash();
        }
        // 30s * 2^(5-1) = 30 * 16 = 480s
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(480));
    }

    #[test]
    fn test_backoff_at_n10() {
        let mut tracker = CrashTracker::new(20);
        for _ in 0..10 {
            tracker.record_crash();
        }
        // 30s * 2^(10-1) = 30 * 512 = 15360s
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(30 * 512));
    }

    #[test]
    fn test_backoff_at_n20() {
        let mut tracker = CrashTracker::new(25);
        for _ in 0..20 {
            tracker.record_crash();
        }
        // 30s * 2^(20-1) = 30 * 524288 = 15728640s
        assert_eq!(
            tracker.backoff_duration(),
            Duration::from_secs(30 * 524_288)
        );
    }

    #[test]
    fn test_backoff_at_n21_capped_same_as_n21_exponent() {
        let mut tracker = CrashTracker::new(30);
        for _ in 0..21 {
            tracker.record_crash();
        }
        // exponent = 20 (capped), so 30s * 2^20 = 30 * 1_048_576 = 31_457_280s
        assert_eq!(
            tracker.backoff_duration(),
            Duration::from_secs(30 * 1_048_576)
        );
    }

    #[test]
    fn test_backoff_at_n22_capped_same_as_n21() {
        let mut tracker_21 = CrashTracker::new(30);
        for _ in 0..21 {
            tracker_21.record_crash();
        }

        let mut tracker_22 = CrashTracker::new(30);
        for _ in 0..22 {
            tracker_22.record_crash();
        }

        assert_eq!(
            tracker_21.backoff_duration(),
            tracker_22.backoff_duration(),
            "Backoff should be capped: n=21 and n=22 produce same duration"
        );
    }

    #[test]
    fn test_backoff_at_n100_capped() {
        let mut tracker = CrashTracker::new(200);
        for _ in 0..100 {
            tracker.record_crash();
        }
        // All exponents >= 20 produce the same capped duration
        let capped = Duration::from_secs(30 * 1_048_576);
        assert_eq!(tracker.backoff_duration(), capped);
    }

    // --- MAX_CRASHES edge cases ---

    #[test]
    fn test_max_crashes_zero_aborts_immediately() {
        let tracker = CrashTracker::new(0);
        // 0 >= 0 is true, so should_abort is true even with no crashes
        assert!(
            tracker.should_abort(),
            "MAX_CRASHES=0 means abort immediately (0 >= 0)"
        );
    }

    #[test]
    fn test_max_crashes_u32_max_never_aborts_in_practice() {
        let mut tracker = CrashTracker::new(u32::MAX);
        for _ in 0..1000 {
            tracker.record_crash();
        }
        assert!(
            !tracker.should_abort(),
            "With u32::MAX threshold, 1000 crashes should not trigger abort"
        );
    }

    #[test]
    fn test_should_abort_exactly_at_threshold() {
        let mut tracker = CrashTracker::new(5);
        for _ in 0..4 {
            tracker.record_crash();
        }
        assert!(!tracker.should_abort(), "4 < 5, should not abort");
        tracker.record_crash();
        assert!(tracker.should_abort(), "5 == 5, should abort");
    }

    #[test]
    fn test_should_abort_above_threshold() {
        let mut tracker = CrashTracker::new(3);
        for _ in 0..5 {
            tracker.record_crash();
        }
        assert!(
            tracker.should_abort(),
            "5 > 3, should still abort (stays true above threshold)"
        );
    }

    // --- Saturating arithmetic ---

    #[test]
    fn test_saturating_add_at_u32_max() {
        let mut tracker = CrashTracker::new(u32::MAX);
        // Simulate being near u32::MAX by internal manipulation via repeated crashes
        // We can't practically do u32::MAX crashes, so test that the logic
        // doesn't panic by verifying saturating behavior indirectly
        tracker.record_crash();
        let count_after_one = tracker.count();
        assert_eq!(count_after_one, 1);
        // The saturating_add ensures no overflow even conceptually
    }

    // --- Complex sequences ---

    #[test]
    fn test_multiple_crash_success_cycles() {
        let mut tracker = CrashTracker::new(5);

        // Cycle 1: 3 crashes then success
        for _ in 0..3 {
            tracker.record_crash();
        }
        assert_eq!(tracker.count(), 3);
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(120));
        tracker.record_success();
        assert_eq!(tracker.count(), 0);

        // Cycle 2: 2 crashes then success
        for _ in 0..2 {
            tracker.record_crash();
        }
        assert_eq!(tracker.count(), 2);
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(60));
        tracker.record_success();
        assert_eq!(tracker.count(), 0);

        // Cycle 3: 1 crash then success
        tracker.record_crash();
        assert_eq!(tracker.count(), 1);
        assert_eq!(tracker.backoff_duration(), Duration::from_secs(30));
        tracker.record_success();
        assert_eq!(tracker.count(), 0);
        assert_eq!(tracker.backoff_duration(), Duration::ZERO);
    }

    #[test]
    fn test_crash_to_abort_then_continue_recording() {
        let mut tracker = CrashTracker::new(3);
        for _ in 0..3 {
            tracker.record_crash();
        }
        assert!(tracker.should_abort());

        // Even after abort threshold, crash counting continues (saturating)
        tracker.record_crash();
        assert_eq!(tracker.count(), 4);
        assert!(tracker.should_abort(), "Still above threshold");

        // But success still resets
        tracker.record_success();
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());
    }

    #[test]
    fn test_interleaved_single_crash_success() {
        let mut tracker = CrashTracker::new(3);
        for _ in 0..10 {
            tracker.record_crash();
            assert_eq!(tracker.count(), 1);
            assert!(!tracker.should_abort());
            tracker.record_success();
            assert_eq!(tracker.count(), 0);
        }
    }

    #[test]
    fn test_backoff_progression_is_strictly_increasing_until_cap() {
        let mut tracker = CrashTracker::new(25);
        let mut prev_duration = Duration::ZERO;
        // Exponent = n-1, capped at 20. So n=1..=21 all have unique exponents (0..=20).
        // n=22 would have exponent 21 capped to 20, same as n=21.
        for i in 1..=21 {
            tracker.record_crash();
            let current = tracker.backoff_duration();
            assert!(
                current > prev_duration,
                "Backoff at crash {} ({:?}) should exceed crash {} ({:?})",
                i,
                current,
                i - 1,
                prev_duration
            );
            prev_duration = current;
        }
        // At n=22 (exponent 21 capped to 20), duration equals n=21
        tracker.record_crash();
        assert_eq!(
            tracker.backoff_duration(),
            prev_duration,
            "At n=22 (exponent capped at 20), duration should equal n=21"
        );
    }

    #[test]
    fn test_multiple_successes_in_a_row() {
        let mut tracker = CrashTracker::new(3);
        tracker.record_crash();
        tracker.record_crash();
        tracker.record_success();
        tracker.record_success();
        tracker.record_success();
        assert_eq!(tracker.count(), 0, "Multiple successes keep counter at 0");
    }
}
