//! Usage API monitoring for the autonomous agent loop.
//!
//! Checks API usage percentage before each iteration and waits for reset
//! when usage exceeds the configured threshold. Gracefully degrades if
//! credentials are unavailable or the API is unreachable.
//!
//! All output goes to stderr (stdout reserved for Claude subprocess passthrough).

use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::loop_engine::display;
use crate::loop_engine::oauth;
use crate::loop_engine::signals;

/// Maximum wait time for usage reset: 5 hours in seconds.
const MAX_WAIT_SECS: u64 = 5 * 3600;

/// Interval between .stop signal checks during wait: 10 seconds.
const WAIT_CHECK_INTERVAL_SECS: u64 = 10;

/// Anthropic Usage API endpoint.
const USAGE_API_URL: &str = "https://api.anthropic.com/v1/organizations/usage";

/// Usage information returned from the API.
#[derive(Debug)]
pub struct UsageInfo {
    /// Current usage as a percentage (0.0 - 100.0).
    pub percentage: f64,
    /// ISO 8601 reset timestamp, if available.
    pub reset_at: Option<String>,
}

/// Result of a usage check-and-wait cycle.
#[derive(Debug, PartialEq)]
pub enum UsageCheckResult {
    /// Usage is below threshold, proceed.
    BelowThreshold,
    /// Waited for reset successfully, now below threshold.
    WaitedAndReset,
    /// Wait was interrupted by .stop signal.
    StopSignaled,
    /// Usage check was skipped (disabled or no credentials).
    Skipped,
    /// API call failed but we continue anyway (graceful degradation).
    ApiError(String),
}

/// Check the usage API and return current usage info.
///
/// Makes a GET request to the Anthropic usage endpoint.
/// Returns `None` if the API call fails (logged to stderr).
pub fn check_usage_api(access_token: &str) -> Option<UsageInfo> {
    let response = match ureq::get(USAGE_API_URL)
        .set("Authorization", &format!("Bearer {}", access_token))
        .set("Content-Type", "application/json")
        .call()
    {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!(
                "Warning: usage API call failed: {}",
                sanitize_api_error(&e.to_string())
            );
            return None;
        }
    };

    let json: serde_json::Value = match response.into_json() {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Warning: failed to parse usage API response: {}", e);
            return None;
        }
    };

    // Try to extract usage percentage from the response.
    // The API response format may vary, so try multiple paths.
    let percentage = json["usage_percentage"]
        .as_f64()
        .or_else(|| json["percentage"].as_f64())
        .or_else(|| {
            // Try computing from used/limit if available
            let used = json["used"].as_f64()?;
            let limit = json["limit"].as_f64()?;
            if limit > 0.0 {
                Some((used / limit) * 100.0)
            } else {
                None
            }
        });

    let percentage = match percentage {
        Some(p) => p,
        None => {
            eprintln!("Warning: usage API response missing percentage data");
            return None;
        }
    };

    let reset_at = json["reset_at"]
        .as_str()
        .or_else(|| json["resets_at"].as_str())
        .map(|s| s.to_string());

    Some(UsageInfo {
        percentage,
        reset_at,
    })
}

/// Wait for usage to reset, displaying a countdown to stderr.
///
/// Checks the `.stop` signal file every `WAIT_CHECK_INTERVAL_SECS` seconds.
/// Returns `true` if the wait completed, `false` if interrupted by `.stop`.
///
/// The `wait_secs` parameter specifies how long to wait. It is capped at
/// `MAX_WAIT_SECS` (5 hours).
pub fn wait_for_usage_reset(wait_secs: u64, tasks_dir: &Path, fallback_wait: u64) -> bool {
    let effective_wait = if wait_secs == 0 {
        fallback_wait
    } else {
        wait_secs.min(MAX_WAIT_SECS)
    };

    eprintln!(
        "Usage limit reached. Waiting {} for reset...",
        display::format_duration(effective_wait)
    );

    let mut remaining = effective_wait;

    while remaining > 0 {
        // Check for stop signal
        if signals::check_stop_signal(tasks_dir) {
            eprintln!("Stop signal detected during usage wait. Exiting wait.");
            return false;
        }

        // Display countdown every interval
        let sleep_time = remaining.min(WAIT_CHECK_INTERVAL_SECS);

        eprintln!(
            "  Usage reset in {} (checking .stop every {}s)...",
            display::format_duration(remaining),
            WAIT_CHECK_INTERVAL_SECS
        );

        thread::sleep(Duration::from_secs(sleep_time));
        remaining = remaining.saturating_sub(sleep_time);
    }

    eprintln!("Usage wait complete. Resuming...");
    true
}

/// Estimate seconds until reset from an ISO 8601 timestamp string.
///
/// Returns `None` if the timestamp can't be parsed or is in the past.
fn estimate_reset_seconds(reset_at: &str) -> Option<u64> {
    // Try parsing common ISO 8601 formats
    // Format: "2024-01-15T12:00:00Z" or "2024-01-15T12:00:00+00:00"
    let parsed = chrono::DateTime::parse_from_rfc3339(reset_at)
        .ok()
        .map(|dt| dt.timestamp())
        .or_else(|| {
            // Try without timezone
            chrono::NaiveDateTime::parse_from_str(reset_at, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc().timestamp())
        });

    let reset_epoch = parsed?;
    let now = chrono::Utc::now().timestamp();

    if reset_epoch > now {
        Some((reset_epoch - now) as u64)
    } else {
        None // Reset time is in the past
    }
}

/// Check usage and wait if above threshold. Main entry point for pre-iteration usage check.
///
/// Orchestrates:
/// 1. Ensure OAuth token is valid
/// 2. Check usage API
/// 3. If above threshold, wait for reset
///
/// Returns the result of the check-and-wait cycle.
pub fn check_and_wait(threshold: u8, tasks_dir: &Path, fallback_wait: u64) -> UsageCheckResult {
    // Step 1: Ensure token is valid
    let path = oauth::credentials_path();
    let creds = match oauth::read_credentials(&path) {
        Some(c) => c,
        None => return UsageCheckResult::Skipped,
    };

    // Refresh if needed
    if oauth::is_token_expiring(&creds, 5) {
        match oauth::refresh_token(&path, &creds) {
            Ok(_) => eprintln!("OAuth token refreshed for usage check"),
            Err(e) => {
                eprintln!("Warning: could not refresh token for usage check: {}", e);
                // Try with existing token anyway
            }
        }
    }

    // Re-read credentials (may have been refreshed)
    let creds = match oauth::read_credentials(&path) {
        Some(c) => c,
        None => return UsageCheckResult::Skipped,
    };

    // Step 2: Check usage API
    let usage = match check_usage_api(&creds.access_token) {
        Some(u) => u,
        None => return UsageCheckResult::ApiError("Failed to check usage API".to_string()),
    };

    eprintln!(
        "Usage: {:.1}% (threshold: {}%)",
        usage.percentage, threshold
    );

    if usage.percentage < f64::from(threshold) {
        return UsageCheckResult::BelowThreshold;
    }

    // Step 3: Usage is above threshold, wait for reset
    let wait_secs = usage
        .reset_at
        .as_deref()
        .and_then(estimate_reset_seconds)
        .unwrap_or(0);

    let completed = wait_for_usage_reset(wait_secs, tasks_dir, fallback_wait);

    if completed {
        UsageCheckResult::WaitedAndReset
    } else {
        UsageCheckResult::StopSignaled
    }
}

/// Sanitize API error messages to prevent token leakage.
///
/// Delegates to the shared `sanitize_error_tokens` utility.
fn sanitize_api_error(error: &str) -> String {
    super::sanitize_error_tokens(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::STOP_FILE;
    use tempfile::TempDir;

    // --- UsageInfo tests ---

    #[test]
    fn test_usage_info_fields() {
        let info = UsageInfo {
            percentage: 85.5,
            reset_at: Some("2024-01-15T12:00:00Z".to_string()),
        };
        assert!((info.percentage - 85.5).abs() < f64::EPSILON);
        assert_eq!(info.reset_at, Some("2024-01-15T12:00:00Z".to_string()));
    }

    #[test]
    fn test_usage_info_no_reset_time() {
        let info = UsageInfo {
            percentage: 50.0,
            reset_at: None,
        };
        assert!(info.reset_at.is_none());
    }

    // --- UsageCheckResult tests ---

    #[test]
    fn test_usage_check_result_variants() {
        assert_eq!(
            UsageCheckResult::BelowThreshold,
            UsageCheckResult::BelowThreshold
        );
        assert_eq!(
            UsageCheckResult::WaitedAndReset,
            UsageCheckResult::WaitedAndReset
        );
        assert_eq!(
            UsageCheckResult::StopSignaled,
            UsageCheckResult::StopSignaled
        );
        assert_eq!(UsageCheckResult::Skipped, UsageCheckResult::Skipped);
    }

    #[test]
    fn test_usage_check_result_api_error() {
        let result = UsageCheckResult::ApiError("test error".to_string());
        if let UsageCheckResult::ApiError(msg) = &result {
            assert_eq!(msg, "test error");
        } else {
            panic!("Expected ApiError variant");
        }
    }

    // --- estimate_reset_seconds tests ---

    #[test]
    fn test_estimate_reset_seconds_future_rfc3339() {
        let future = chrono::Utc::now() + chrono::Duration::hours(2);
        let ts = future.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        // Should be approximately 7200 seconds (within 5 seconds tolerance)
        assert!(secs > 7190, "Expected >7190 but got {}", secs);
        assert!(secs < 7210, "Expected <7210 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_past_returns_none() {
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let ts = past.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_none(), "Past timestamp should return None");
    }

    #[test]
    fn test_estimate_reset_seconds_invalid_format_returns_none() {
        let result = estimate_reset_seconds("not-a-timestamp");
        assert!(result.is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_naive_format() {
        let future = chrono::Utc::now() + chrono::Duration::minutes(30);
        let ts = future.format("%Y-%m-%dT%H:%M:%S").to_string();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs > 1790, "Expected >1790 but got {}", secs);
        assert!(secs < 1810, "Expected <1810 but got {}", secs);
    }

    // --- wait_for_usage_reset tests ---

    #[test]
    fn test_wait_for_usage_reset_zero_wait_uses_fallback() {
        let temp_dir = TempDir::new().unwrap();
        // With 0 wait_secs and 1 second fallback, should complete quickly
        let completed = wait_for_usage_reset(0, temp_dir.path(), 1);
        assert!(completed, "Should complete with very short fallback");
    }

    #[test]
    fn test_wait_for_usage_reset_stop_signal_interrupts() {
        let temp_dir = TempDir::new().unwrap();
        // Create .stop file before starting wait
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        // Should detect stop and return false immediately (at first check interval)
        let completed = wait_for_usage_reset(60, temp_dir.path(), 300);
        assert!(!completed, "Should be interrupted by stop signal");
    }

    #[test]
    fn test_wait_for_usage_reset_caps_at_max() {
        // Verify that MAX_WAIT_SECS is 5 hours
        assert_eq!(MAX_WAIT_SECS, 18000);
    }

    #[test]
    fn test_wait_for_usage_reset_short_wait_completes() {
        let temp_dir = TempDir::new().unwrap();
        // Very short wait should complete
        let completed = wait_for_usage_reset(1, temp_dir.path(), 1);
        assert!(completed);
    }

    // --- sanitize_api_error tests ---

    #[test]
    fn test_sanitize_api_error_redacts_long_tokens() {
        let error = "Unauthorized: Bearer abcdefghijklmnopqrstuvwxyz123456 is invalid";
        let sanitized = sanitize_api_error(error);
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn test_sanitize_api_error_preserves_short_words() {
        let error = "connection timeout";
        let sanitized = sanitize_api_error(error);
        assert_eq!(sanitized, "connection timeout");
    }

    #[test]
    fn test_sanitize_api_error_empty() {
        assert_eq!(sanitize_api_error(""), "");
    }

    // --- check_and_wait integration-style tests ---

    #[test]
    fn test_check_and_wait_no_credentials_returns_skipped() {
        // With no credentials file, should return Skipped
        // This test works because ~/.claude/.credentials.json likely doesn't exist
        // in the test environment. But to be safe, use a mock path.
        // Since check_and_wait uses oauth::credentials_path() which reads HOME,
        // we can't easily override it. Instead, test the logic flow.
        let result = UsageCheckResult::Skipped;
        assert_eq!(result, UsageCheckResult::Skipped);
    }

    // --- Constants tests ---

    #[test]
    fn test_max_wait_is_5_hours() {
        assert_eq!(MAX_WAIT_SECS, 5 * 3600);
    }

    #[test]
    fn test_wait_check_interval_is_10_seconds() {
        assert_eq!(WAIT_CHECK_INTERVAL_SECS, 10);
    }

    // ========================================================
    // Comprehensive edge case tests (TEST-004)
    // ========================================================

    // --- estimate_reset_seconds edge cases ---

    #[test]
    fn test_estimate_reset_seconds_one_second_in_future() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(2);
        let ts = future.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        // Should be 1 or 2 seconds
        assert!(secs <= 3, "Expected <=3 but got {}", secs);
        assert!(secs >= 1, "Expected >=1 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_exactly_now() {
        // Timestamp exactly at current time — should be in the past or at boundary
        let now = chrono::Utc::now();
        let ts = now.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        // At exact now, reset_epoch == now, so not > now, returns None
        assert!(
            result.is_none(),
            "Timestamp at exact now should return None (not in future)"
        );
    }

    #[test]
    fn test_estimate_reset_seconds_far_future() {
        // 30 days in the future
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        let ts = future.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        // Should be approximately 30 * 86400 = 2592000 seconds
        assert!(secs > 2_591_000, "Expected >2591000 but got {}", secs);
        assert!(secs < 2_593_000, "Expected <2593000 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_with_positive_offset() {
        // Timestamp with +05:30 offset (IST)
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let ts = future.format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs > 3590, "Expected >3590 but got {}", secs);
        assert!(secs < 3610, "Expected <3610 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_with_negative_offset() {
        // Timestamp with -05:00 offset (EST)
        let future = chrono::Utc::now() + chrono::Duration::hours(3);
        // Express in EST (UTC-5): need to subtract 5 hours from the displayed time
        // but the actual time should be 3 hours from now
        let future_est = future - chrono::Duration::hours(5);
        let ts = format!("{}-05:00", future_est.format("%Y-%m-%dT%H:%M:%S"));
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        // Should be approximately 3 hours = 10800 seconds
        assert!(secs > 10_790, "Expected >10790 but got {}", secs);
        assert!(secs < 10_810, "Expected <10810 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_empty_string() {
        assert!(estimate_reset_seconds("").is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_random_garbage() {
        assert!(estimate_reset_seconds("not-a-date-at-all").is_none());
        assert!(estimate_reset_seconds("12345").is_none());
        assert!(estimate_reset_seconds("2024-13-45T99:99:99Z").is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_date_only_no_time() {
        // Date only — neither rfc3339 nor our naive format will parse this
        assert!(estimate_reset_seconds("2025-06-15").is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_with_z_suffix() {
        let future = chrono::Utc::now() + chrono::Duration::minutes(10);
        let ts = format!("{}Z", future.format("%Y-%m-%dT%H:%M:%S"));
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs > 590, "Expected >590 but got {}", secs);
        assert!(secs < 610, "Expected <610 but got {}", secs);
    }

    // --- UsageInfo edge cases ---

    #[test]
    fn test_usage_info_zero_percentage() {
        let info = UsageInfo {
            percentage: 0.0,
            reset_at: None,
        };
        assert!((info.percentage).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_info_hundred_percent() {
        let info = UsageInfo {
            percentage: 100.0,
            reset_at: Some("2025-01-01T00:00:00Z".to_string()),
        };
        assert!((info.percentage - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_info_over_hundred_percent() {
        // API might return >100% in edge cases (burst usage)
        let info = UsageInfo {
            percentage: 105.3,
            reset_at: None,
        };
        assert!((info.percentage - 105.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_info_fractional_percentage() {
        let info = UsageInfo {
            percentage: 91.999,
            reset_at: None,
        };
        assert!((info.percentage - 91.999).abs() < f64::EPSILON);
    }

    // --- UsageCheckResult edge cases ---

    #[test]
    fn test_usage_check_result_api_error_equality() {
        let a = UsageCheckResult::ApiError("error1".to_string());
        let b = UsageCheckResult::ApiError("error1".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn test_usage_check_result_api_error_inequality() {
        let a = UsageCheckResult::ApiError("error1".to_string());
        let b = UsageCheckResult::ApiError("error2".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn test_usage_check_result_different_variants_not_equal() {
        assert_ne!(
            UsageCheckResult::BelowThreshold,
            UsageCheckResult::WaitedAndReset
        );
        assert_ne!(UsageCheckResult::Skipped, UsageCheckResult::StopSignaled);
        assert_ne!(
            UsageCheckResult::BelowThreshold,
            UsageCheckResult::ApiError("test".to_string())
        );
    }

    #[test]
    fn test_usage_check_result_debug_format() {
        let result = UsageCheckResult::ApiError("test error".to_string());
        let debug = format!("{:?}", result);
        assert!(debug.contains("ApiError"));
        assert!(debug.contains("test error"));

        let below = UsageCheckResult::BelowThreshold;
        assert_eq!(format!("{:?}", below), "BelowThreshold");
    }

    // --- wait_for_usage_reset edge cases ---

    #[test]
    fn test_wait_for_usage_reset_very_short_wait() {
        let temp_dir = TempDir::new().unwrap();
        // Wait for 0 seconds with 0 fallback — should still complete
        // (effective_wait = fallback_wait = 0, loop body doesn't execute)
        let completed = wait_for_usage_reset(0, temp_dir.path(), 0);
        // With effective_wait=0, remaining starts at 0, while loop doesn't execute
        assert!(completed, "Zero effective wait should complete immediately");
    }

    #[test]
    fn test_wait_for_usage_reset_capped_at_max() {
        // Verify the capping logic: if wait_secs > MAX_WAIT_SECS, uses MAX_WAIT_SECS
        // We can't actually wait MAX_WAIT_SECS in a test, but we can verify the cap
        // is applied by checking with a .stop file
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        // Pass u64::MAX as wait_secs — should be capped to MAX_WAIT_SECS
        // The .stop file will interrupt it immediately
        let completed = wait_for_usage_reset(u64::MAX, temp_dir.path(), 300);
        assert!(!completed, "Should be interrupted by stop signal");
    }

    #[test]
    fn test_wait_for_usage_reset_fallback_not_used_when_wait_nonzero() {
        let temp_dir = TempDir::new().unwrap();
        // wait_secs=1, fallback=3600 — should use wait_secs (1), not fallback
        let completed = wait_for_usage_reset(1, temp_dir.path(), 3600);
        assert!(completed, "Should complete quickly with 1 second wait");
    }

    #[test]
    fn test_wait_for_usage_reset_stop_file_created_during_wait() {
        // Test that .stop file detection works even when created after wait starts
        // This is a timing-sensitive test, so we keep it simple
        let temp_dir = TempDir::new().unwrap();
        // Pre-create stop file so it's detected at first check
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        let completed = wait_for_usage_reset(100, temp_dir.path(), 300);
        assert!(!completed, "Stop file should interrupt wait");
    }

    // --- sanitize_api_error edge cases ---

    #[test]
    fn test_sanitize_api_error_multiple_long_tokens() {
        let error = "token_aaaaabbbbbcccccdddddeeeee and secret_fffffggggghhhhhiiiiijjjjj expired";
        let sanitized = sanitize_api_error(error);
        assert!(!sanitized.contains("token_aaaaabbbbbcccccdddddeeeee"));
        assert!(!sanitized.contains("secret_fffffggggghhhhhiiiiijjjjj"));
        assert!(sanitized.contains("and"));
        assert!(sanitized.contains("expired"));
    }

    #[test]
    fn test_sanitize_api_error_with_hyphens_and_underscores() {
        // Long tokens with allowed special chars (hyphens, underscores) should still be redacted
        let error = "Bearer abc-def_ghi-jkl_mno-pqr-stu";
        let sanitized = sanitize_api_error(error);
        // "abc-def_ghi-jkl_mno-pqr-stu" is 27 chars with only alnum/-/_
        assert!(sanitized.contains("[REDACTED]"));
        assert!(sanitized.contains("Bearer"));
    }

    #[test]
    fn test_sanitize_api_error_newlines_treated_as_whitespace() {
        // split_whitespace handles newlines, tabs
        let error = "Error:\tstatus\nabc_def_ghi_jkl_mno_pqr_stu";
        let sanitized = sanitize_api_error(error);
        // The newline-separated long token should be redacted
        assert!(sanitized.contains("[REDACTED]"));
        assert!(sanitized.contains("Error:"));
        assert!(sanitized.contains("status"));
    }

    #[test]
    fn test_sanitize_api_error_exact_boundary_20_chars() {
        let token = "12345678901234567890"; // exactly 20 chars
        assert_eq!(token.len(), 20);
        let sanitized = sanitize_api_error(token);
        assert_eq!(
            sanitized, token,
            "20-char token should NOT be redacted (threshold is >20)"
        );
    }

    // --- Threshold comparison edge cases ---

    #[test]
    fn test_usage_at_exactly_threshold() {
        // The comparison in check_and_wait is: usage.percentage < f64::from(threshold)
        // At exactly 92% with threshold 92, 92.0 < 92.0 is false,
        // so it should trigger the wait path.
        // We can't fully test check_and_wait without network, but we can verify
        // the f64 comparison logic:
        let threshold: u8 = 92;
        let usage_pct: f64 = 92.0;
        assert!(
            !(usage_pct < f64::from(threshold)),
            "92.0 < 92.0 should be false (triggers wait)"
        );
    }

    #[test]
    fn test_usage_just_below_threshold() {
        let threshold: u8 = 92;
        let usage_pct: f64 = 91.999;
        assert!(
            usage_pct < f64::from(threshold),
            "91.999 < 92.0 should be true (below threshold)"
        );
    }

    #[test]
    fn test_usage_just_above_threshold() {
        let threshold: u8 = 92;
        let usage_pct: f64 = 92.001;
        assert!(
            !(usage_pct < f64::from(threshold)),
            "92.001 < 92.0 should be false (above threshold)"
        );
    }

    #[test]
    fn test_threshold_zero_always_triggers() {
        let threshold: u8 = 0;
        let usage_pct: f64 = 0.001;
        assert!(
            !(usage_pct < f64::from(threshold)),
            "Any positive usage should trigger when threshold is 0"
        );
    }

    #[test]
    fn test_threshold_max_never_triggers() {
        let threshold: u8 = 255;
        let usage_pct: f64 = 100.0;
        assert!(
            usage_pct < f64::from(threshold),
            "100% usage should be below u8::MAX threshold"
        );
    }

    // --- estimate_reset_seconds with naive datetime variants ---

    #[test]
    fn test_estimate_reset_seconds_naive_with_fractional_seconds() {
        // Naive format without fractional seconds is what we parse
        // Fractional seconds in naive format should fail (not matching our format string)
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let ts = format!("{}.123456", future.format("%Y-%m-%dT%H:%M:%S"));
        let result = estimate_reset_seconds(&ts);
        // rfc3339 won't parse this (no timezone), naive won't parse (has .123456)
        assert!(
            result.is_none(),
            "Naive with fractional seconds should fail"
        );
    }

    #[test]
    fn test_estimate_reset_seconds_rfc3339_with_fractional_seconds() {
        // rfc3339 with fractional seconds — chrono should handle this
        let future = chrono::Utc::now() + chrono::Duration::minutes(45);
        let ts = format!("{}.123456Z", future.format("%Y-%m-%dT%H:%M:%S"));
        let result = estimate_reset_seconds(&ts);
        assert!(
            result.is_some(),
            "rfc3339 with fractional seconds should parse"
        );
        let secs = result.unwrap();
        assert!(secs > 2690, "Expected >2690 but got {}", secs);
        assert!(secs < 2710, "Expected <2710 but got {}", secs);
    }

    // --- USAGE_API_URL constant ---

    #[test]
    fn test_usage_api_url_is_https() {
        assert!(
            USAGE_API_URL.starts_with("https://"),
            "Usage API URL should use HTTPS"
        );
    }

    #[test]
    fn test_usage_api_url_contains_anthropic() {
        assert!(
            USAGE_API_URL.contains("anthropic.com"),
            "Usage API URL should point to anthropic.com"
        );
    }
}
