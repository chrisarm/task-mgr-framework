//! Usage API monitoring for the autonomous agent loop.
//!
//! Checks API usage percentage before each iteration and waits for reset
//! when usage exceeds the configured threshold. Gracefully degrades if
//! credentials are unavailable or the API is unreachable.
//!
//! All output goes to stderr (stdout reserved for Claude subprocess passthrough).
//!
//! CLEANUP-001: `check_and_wait`, `wait_for_usage_reset`, `parse_reset_from_output`,
//! and `estimate_reset_seconds` have been relocated to
//! `reactions::account` where they are called directly by the coordinator.

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
            tracing::warn!(
                error = %sanitize_api_error(&e.to_string()),
                "usage API call failed",
            );
            return None;
        }
    };

    let json: serde_json::Value = match response.into_json() {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse usage API response");
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
            tracing::warn!("usage API response missing percentage data");
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

/// Sanitize API error messages to prevent token leakage.
///
/// Delegates to the shared `sanitize_error_tokens` utility.
fn sanitize_api_error(error: &str) -> String {
    super::sanitize_error_tokens(error)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- Threshold comparison edge cases ---

    #[test]
    fn test_usage_at_exactly_threshold() {
        let threshold: u8 = 92;
        let usage_pct: f64 = 92.0;
        assert!(
            usage_pct >= f64::from(threshold),
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
            usage_pct >= f64::from(threshold),
            "92.001 < 92.0 should be false (above threshold)"
        );
    }

    #[test]
    fn test_threshold_zero_always_triggers() {
        let threshold: u8 = 0;
        let usage_pct: f64 = 0.001;
        assert!(
            usage_pct >= f64::from(threshold),
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
}
