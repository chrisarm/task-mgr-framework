//! Stale iteration tracker for detecting lack of progress.
//!
//! Compares a hash of task state before and after each iteration.
//! If the hash is unchanged, the iteration made no progress (stale).
//! After a configurable number of consecutive stale iterations, signals abort.

/// Tracks consecutive stale iterations by comparing state hashes.
pub struct StaleTracker {
    /// Number of consecutive stale iterations
    consecutive_stale: u32,
    /// Maximum allowed consecutive stale iterations before abort
    threshold: u32,
}

impl StaleTracker {
    /// Create a new StaleTracker with the default threshold of 3.
    pub fn new() -> Self {
        Self {
            consecutive_stale: 0,
            threshold: 3,
        }
    }

    /// Check whether the iteration was stale by comparing before/after hashes.
    ///
    /// If hashes are equal (no progress), increments the stale counter.
    /// If hashes differ (progress was made), resets the counter to 0.
    pub fn check(&mut self, hash_before: &str, hash_after: &str) {
        if hash_before == hash_after {
            self.consecutive_stale = self.consecutive_stale.saturating_add(1);
        } else {
            self.consecutive_stale = 0;
        }
    }

    /// Whether the loop should abort due to too many consecutive stale iterations.
    pub fn should_abort(&self) -> bool {
        self.consecutive_stale >= self.threshold
    }

    /// Current number of consecutive stale iterations.
    pub fn count(&self) -> u32 {
        self.consecutive_stale
    }
}

impl Default for StaleTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AC 4: Same hash increments, different hash resets ---

    #[test]
    fn test_same_hash_increments_counter() {
        let mut tracker = StaleTracker::new();
        tracker.check("abc123", "abc123");
        assert_eq!(tracker.count(), 1, "Same hash should increment to 1");
    }

    #[test]
    fn test_different_hash_resets_counter() {
        let mut tracker = StaleTracker::new();
        tracker.check("abc123", "abc123");
        assert_eq!(tracker.count(), 1);

        tracker.check("abc123", "def456");
        assert_eq!(
            tracker.count(),
            0,
            "Different hash should reset counter to 0"
        );
    }

    #[test]
    fn test_consecutive_same_hashes_accumulate() {
        let mut tracker = StaleTracker::new();
        tracker.check("hash1", "hash1");
        assert_eq!(tracker.count(), 1);
        tracker.check("hash2", "hash2");
        assert_eq!(tracker.count(), 2);
        tracker.check("hash3", "hash3");
        assert_eq!(tracker.count(), 3);
    }

    #[test]
    fn test_different_hash_after_accumulation_resets() {
        let mut tracker = StaleTracker::new();
        tracker.check("aaa", "aaa");
        tracker.check("bbb", "bbb");
        assert_eq!(tracker.count(), 2);

        tracker.check("before", "after");
        assert_eq!(
            tracker.count(),
            0,
            "Different hash should reset even after accumulation"
        );
    }

    // --- AC 5: Abort after 3 consecutive stale iterations ---

    #[test]
    fn test_should_abort_after_3_stale() {
        let mut tracker = StaleTracker::new();
        tracker.check("a", "a");
        assert!(!tracker.should_abort(), "1 stale should not abort");
        tracker.check("b", "b");
        assert!(!tracker.should_abort(), "2 stale should not abort");
        tracker.check("c", "c");
        assert!(tracker.should_abort(), "3 stale should abort");
    }

    #[test]
    fn test_should_not_abort_before_threshold() {
        let mut tracker = StaleTracker::new();
        tracker.check("x", "x");
        tracker.check("y", "y");
        assert!(
            !tracker.should_abort(),
            "2 stale should not abort (threshold=3)"
        );
    }

    #[test]
    fn test_should_not_abort_when_reset_before_threshold() {
        let mut tracker = StaleTracker::new();
        tracker.check("a", "a");
        tracker.check("b", "b");
        // Progress made - resets counter
        tracker.check("before", "after");
        tracker.check("c", "c");
        assert!(
            !tracker.should_abort(),
            "Reset broke the streak, only 1 stale now"
        );
    }

    #[test]
    fn test_abort_requires_consecutive_stale() {
        let mut tracker = StaleTracker::new();
        tracker.check("a", "a"); // stale 1
        tracker.check("b", "b"); // stale 2
        tracker.check("x", "y"); // progress - resets
        tracker.check("c", "c"); // stale 1 again
        assert!(
            !tracker.should_abort(),
            "Non-consecutive stale should not trigger abort"
        );
    }

    // --- Edge cases ---

    #[test]
    fn test_new_tracker_starts_at_zero() {
        let tracker = StaleTracker::new();
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());
    }

    #[test]
    fn test_empty_hash_strings() {
        let mut tracker = StaleTracker::new();
        tracker.check("", "");
        assert_eq!(
            tracker.count(),
            1,
            "Empty equal hashes should count as stale"
        );
    }

    #[test]
    fn test_empty_vs_nonempty_hash() {
        let mut tracker = StaleTracker::new();
        tracker.check("", "something");
        assert_eq!(tracker.count(), 0, "Empty vs non-empty should reset");
    }

    #[test]
    fn test_many_consecutive_stale_iterations() {
        let mut tracker = StaleTracker::new();
        for i in 0..100 {
            tracker.check("same", "same");
            if i >= 2 {
                assert!(
                    tracker.should_abort(),
                    "Should abort from iteration 3 onward"
                );
            }
        }
        assert_eq!(tracker.count(), 100);
    }

    #[test]
    fn test_stale_progress_stale_sequence() {
        let mut tracker = StaleTracker::new();
        // 2 stale
        tracker.check("h1", "h1");
        tracker.check("h2", "h2");
        assert_eq!(tracker.count(), 2);
        // progress
        tracker.check("h2", "h3");
        assert_eq!(tracker.count(), 0);
        // 2 stale again
        tracker.check("h4", "h4");
        tracker.check("h5", "h5");
        assert_eq!(tracker.count(), 2);
        assert!(!tracker.should_abort());
    }

    #[test]
    fn test_default_impl() {
        let tracker = StaleTracker::default();
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());
    }

    // === Comprehensive tests (TEST-002) ===

    // --- Identical hashes across many iterations ---

    #[test]
    fn test_identical_hashes_500_iterations() {
        let mut tracker = StaleTracker::new();
        for i in 1..=500 {
            tracker.check("deadbeef", "deadbeef");
            assert_eq!(tracker.count(), i);
            if i >= 3 {
                assert!(tracker.should_abort());
            }
        }
    }

    #[test]
    fn test_count_uses_saturating_add() {
        // Verify the counter doesn't panic even at high counts
        let mut tracker = StaleTracker::new();
        for _ in 0..10_000 {
            tracker.check("same", "same");
        }
        assert_eq!(tracker.count(), 10_000);
        assert!(tracker.should_abort());
    }

    // --- Hash change after 2 stale resets counter ---

    #[test]
    fn test_hash_change_at_exactly_2_stale_resets() {
        let mut tracker = StaleTracker::new();
        tracker.check("h1", "h1"); // stale 1
        tracker.check("h2", "h2"); // stale 2
        assert_eq!(tracker.count(), 2);
        assert!(!tracker.should_abort());

        tracker.check("before", "after"); // progress
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());
    }

    #[test]
    fn test_hash_change_at_exactly_3_stale_prevents_abort() {
        let mut tracker = StaleTracker::new();
        tracker.check("a", "a"); // stale 1
        tracker.check("b", "b"); // stale 2
        assert!(!tracker.should_abort());

        // Progress right before threshold
        tracker.check("old", "new");
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());

        // Start counting again
        tracker.check("c", "c"); // stale 1
        tracker.check("d", "d"); // stale 2
        assert!(!tracker.should_abort());
    }

    // --- Unicode and special hash strings ---

    #[test]
    fn test_unicode_hash_strings() {
        let mut tracker = StaleTracker::new();
        tracker.check("日本語ハッシュ", "日本語ハッシュ");
        assert_eq!(tracker.count(), 1, "Unicode equal hashes should be stale");

        tracker.check("日本語ハッシュ", "異なるハッシュ");
        assert_eq!(tracker.count(), 0, "Unicode different hashes should reset");
    }

    #[test]
    fn test_very_long_hash_strings() {
        let long_hash = "a".repeat(10_000);
        let mut tracker = StaleTracker::new();
        tracker.check(&long_hash, &long_hash);
        assert_eq!(tracker.count(), 1, "Long equal hashes should be stale");

        let different_long = "b".repeat(10_000);
        tracker.check(&long_hash, &different_long);
        assert_eq!(tracker.count(), 0, "Long different hashes should reset");
    }

    #[test]
    fn test_whitespace_only_hashes() {
        let mut tracker = StaleTracker::new();
        tracker.check("   ", "   ");
        assert_eq!(tracker.count(), 1, "Whitespace-only equal hashes are stale");

        tracker.check("   ", "\t\t\t");
        assert_eq!(
            tracker.count(),
            0,
            "Different whitespace types should reset"
        );
    }

    #[test]
    fn test_case_sensitive_hashes() {
        let mut tracker = StaleTracker::new();
        tracker.check("ABC", "abc");
        assert_eq!(
            tracker.count(),
            0,
            "Hash comparison should be case-sensitive"
        );

        tracker.check("ABC", "ABC");
        assert_eq!(tracker.count(), 1);
    }

    // --- Complex reset/stale patterns ---

    #[test]
    fn test_alternating_stale_and_progress() {
        let mut tracker = StaleTracker::new();
        for _ in 0..20 {
            tracker.check("same", "same"); // stale
            assert_eq!(tracker.count(), 1);
            tracker.check("old", "new"); // progress
            assert_eq!(tracker.count(), 0);
        }
        assert!(
            !tracker.should_abort(),
            "Alternating never reaches threshold"
        );
    }

    #[test]
    fn test_two_stale_then_progress_repeated() {
        let mut tracker = StaleTracker::new();
        for _ in 0..10 {
            tracker.check("a", "a"); // stale 1
            tracker.check("b", "b"); // stale 2
            assert_eq!(tracker.count(), 2);
            assert!(!tracker.should_abort());
            tracker.check("x", "y"); // progress resets
            assert_eq!(tracker.count(), 0);
        }
    }

    #[test]
    fn test_stale_exactly_at_threshold_then_progress() {
        let mut tracker = StaleTracker::new();
        tracker.check("a", "a"); // stale 1
        tracker.check("b", "b"); // stale 2
        tracker.check("c", "c"); // stale 3
        assert!(tracker.should_abort(), "At threshold, should abort");

        // But if we get progress, it resets
        tracker.check("old", "new");
        assert_eq!(tracker.count(), 0);
        assert!(
            !tracker.should_abort(),
            "Progress after abort resets tracker"
        );
    }

    #[test]
    fn test_stale_beyond_threshold_then_progress_resets() {
        let mut tracker = StaleTracker::new();
        for _ in 0..10 {
            tracker.check("stale", "stale");
        }
        assert_eq!(tracker.count(), 10);
        assert!(tracker.should_abort());

        // One progress check resets everything
        tracker.check("before", "after");
        assert_eq!(tracker.count(), 0);
        assert!(!tracker.should_abort());
    }

    // --- Different before/after pairs that happen to be equal ---

    #[test]
    fn test_different_equal_pairs_still_count_as_stale() {
        let mut tracker = StaleTracker::new();
        // Each iteration has a different hash value, but before==after each time
        tracker.check("hash_v1", "hash_v1"); // stale
        tracker.check("hash_v2", "hash_v2"); // stale
        tracker.check("hash_v3", "hash_v3"); // stale
        assert_eq!(tracker.count(), 3);
        assert!(
            tracker.should_abort(),
            "Even though the hash values changed iteration-to-iteration, \
             each iteration's before==after means stale"
        );
    }

    // --- Boundary: threshold of 3 ---

    #[test]
    fn test_threshold_boundary_at_2_and_3() {
        let mut tracker = StaleTracker::new();
        tracker.check("x", "x");
        tracker.check("y", "y");
        assert_eq!(tracker.count(), 2);
        assert!(!tracker.should_abort(), "2 < 3 threshold, should not abort");

        tracker.check("z", "z");
        assert_eq!(tracker.count(), 3);
        assert!(tracker.should_abort(), "3 >= 3 threshold, should abort");
    }

    #[test]
    fn test_progress_then_immediate_stale() {
        let mut tracker = StaleTracker::new();
        tracker.check("a", "b"); // progress
        assert_eq!(tracker.count(), 0);

        tracker.check("c", "c"); // stale
        assert_eq!(tracker.count(), 1);
        assert!(!tracker.should_abort());
    }
}
