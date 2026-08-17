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
//!
//! The authoritative consumer/Max usage endpoint is the OAuth HUD API
//! (`GET /api/oauth/usage`) that Claude Code's `/usage` slash command uses.
//! It returns per-window utilization + `resets_at` for the 5-hour session and
//! weekly buckets. The older org usage endpoint is kept as a fallback.

/// Claude Code OAuth usage endpoint (matches `/usage` HUD).
const OAUTH_USAGE_API_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Beta header required by the OAuth usage endpoint.
const OAUTH_USAGE_BETA: &str = "oauth-2025-04-20";

/// Legacy org-level usage endpoint (API-key / org accounts).
const ORG_USAGE_API_URL: &str = "https://api.anthropic.com/v1/organizations/usage";

/// Usage information returned from the API.
#[derive(Debug, Clone)]
pub struct UsageInfo {
    /// Current usage as a percentage (0.0 - 100.0).
    ///
    /// For the OAuth endpoint: **max** utilization across known time windows
    /// (session five-hour, weekly, `limits[]`) so the pre-iteration gate
    /// fires when *any* window is near capacity.
    pub percentage: f64,
    /// ISO 8601 reset timestamp for waiting, if available.
    ///
    /// For the OAuth endpoint: soonest `resets_at` among **exhausted** windows
    /// (util ≥ 100 or severity critical); if none exhausted, prefer
    /// `five_hour.resets_at`, else soonest any window.
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
/// Prefers the OAuth `/api/oauth/usage` endpoint (Claude Code Max/Pro session
/// + weekly windows — same source as `/usage`).
///
/// Falls back to the org usage endpoint when the OAuth call fails (API-key
/// accounts, older responses).
///
/// Returns `None` if both calls fail (logged via tracing).
pub fn check_usage_api(access_token: &str) -> Option<UsageInfo> {
    match fetch_oauth_usage(access_token) {
        Some(info) => Some(info),
        None => fetch_org_usage(access_token),
    }
}

/// Fetch Claude Code OAuth usage (five_hour / seven_day / limits[]).
fn fetch_oauth_usage(access_token: &str) -> Option<UsageInfo> {
    let mut response = match ureq::get(OAUTH_USAGE_API_URL)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("anthropic-beta", OAUTH_USAGE_BETA)
        .header("Content-Type", "application/json")
        .call()
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(
                error = %sanitize_api_error(&e.to_string()),
                "oauth usage API call failed",
            );
            return None;
        }
    };

    let json: serde_json::Value = match response.body_mut().read_json() {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse oauth usage API response");
            return None;
        }
    };

    parse_oauth_usage_json(&json)
}

/// Fetch legacy org-level usage endpoint.
fn fetch_org_usage(access_token: &str) -> Option<UsageInfo> {
    let mut response = match ureq::get(ORG_USAGE_API_URL)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .call()
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(
                error = %sanitize_api_error(&e.to_string()),
                "org usage API call failed",
            );
            return None;
        }
    };

    let json: serde_json::Value = match response.body_mut().read_json() {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse org usage API response");
            return None;
        }
    };

    parse_org_usage_json(&json)
}

/// One utilization window extracted from the OAuth usage payload.
struct UsageWindow {
    util: f64,
    reset: Option<String>,
    /// util ≥ 100, or `limits[].severity == "critical"`, or percent ≥ 100.
    exhausted: bool,
    /// Named `five_hour` bucket (session) — preferred when nothing is exhausted.
    is_five_hour: bool,
}

/// Parse the Claude Code OAuth usage JSON into [`UsageInfo`].
///
/// Hybrid semantics (plan v2):
/// - **percentage** = max utilization across all known time windows
/// - **reset_at** = soonest among *exhausted* windows; if none, prefer
///   `five_hour.resets_at`; else soonest any
///
/// Pure / unit-testable — no I/O.
pub(crate) fn parse_oauth_usage_json(json: &serde_json::Value) -> Option<UsageInfo> {
    let mut windows: Vec<UsageWindow> = Vec::new();

    // Named buckets Claude Code exposes.
    for key in [
        "five_hour",
        "seven_day",
        "seven_day_opus",
        "seven_day_sonnet",
    ] {
        if let Some(bucket) = json.get(key)
            && let Some(util) = bucket_utilization(bucket)
        {
            let reset = bucket
                .get("resets_at")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            windows.push(UsageWindow {
                util,
                reset,
                exhausted: util >= 100.0,
                is_five_hour: key == "five_hour",
            });
        }
    }

    // Structured limits array (severity / percent / kind).
    if let Some(limits) = json.get("limits").and_then(|v| v.as_array()) {
        for limit in limits {
            let percent = limit.get("percent").and_then(|v| v.as_f64()).or_else(|| {
                limit
                    .get("percent")
                    .and_then(|v| v.as_u64())
                    .map(|u| u as f64)
            });
            let Some(p) = percent else {
                continue;
            };
            let reset = limit
                .get("resets_at")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let critical = limit
                .get("severity")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("critical"));
            windows.push(UsageWindow {
                util: p,
                reset,
                exhausted: p >= 100.0 || critical,
                is_five_hour: false,
            });
        }
    }

    if windows.is_empty() {
        tracing::warn!("oauth usage API response missing utilization windows");
        return None;
    }

    let percentage = windows.iter().map(|w| w.util).fold(0.0_f64, f64::max);

    let reset_at = soonest_reset(windows.iter().filter(|w| w.exhausted).map(|w| &w.reset))
        .or_else(|| {
            windows
                .iter()
                .find(|w| w.is_five_hour)
                .and_then(|w| w.reset.clone())
        })
        .or_else(|| soonest_reset(windows.iter().map(|w| &w.reset)));

    Some(UsageInfo {
        percentage,
        reset_at,
    })
}

fn bucket_utilization(bucket: &serde_json::Value) -> Option<f64> {
    // Live API uses 0–100. Some clients report 0.0–1.0 fractions.
    // Heuristic: values ≤ 1.0 are treated as fractions (so 1.0 → 100%).
    let raw = bucket.get("utilization")?.as_f64()?;
    if raw <= 1.0 {
        Some(raw * 100.0)
    } else {
        Some(raw)
    }
}

/// Pick the chronologically soonest ISO-8601 reset timestamp from an iterator
/// of optional strings. Invalid / unparseable timestamps are skipped.
fn soonest_reset<'a>(resets: impl Iterator<Item = &'a Option<String>>) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for reset in resets.flatten() {
        let ts = chrono::DateTime::parse_from_rfc3339(reset)
            .ok()
            .map(|dt| dt.timestamp())
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(reset, "%Y-%m-%dT%H:%M:%S")
                    .ok()
                    .map(|dt| dt.and_utc().timestamp())
            });
        if let Some(ts) = ts {
            match &best {
                Some((best_ts, _)) if ts >= *best_ts => {}
                _ => best = Some((ts, reset.clone())),
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Single chokepoint: credentials path → read → optional refresh → usage API.
///
/// Used by the pre-iteration gate, post-rate-limit resolve, spillover blackout
/// duration, and early-lift probes. Returns `None` when credentials are missing
/// or both usage endpoints fail.
pub fn load_usage_info() -> Option<UsageInfo> {
    let path = super::oauth::credentials_path();
    let mut creds = super::oauth::read_credentials(&path)?;
    if super::oauth::is_token_expiring(&creds, 5) {
        match super::oauth::refresh_token(&path, &creds) {
            Ok(refreshed) => {
                crate::output::ui::emit("OAuth token refreshed for usage check");
                creds = refreshed;
            }
            Err(e) => {
                crate::output::ui::emit_err(&format!(
                    "Warning: could not refresh token for usage check: {}",
                    e
                ));
                // Try with existing token anyway.
            }
        }
    }
    check_usage_api(&creds.access_token)
}

/// Whether an early-lift probe should treat the account as recovered.
///
/// - Pre-gate: percentage below the configured threshold, OR reset is ready
///   (`estimate` would be 0 — caller may pass `reset_ready`).
/// - Post-limit: percentage dropped below 95 (window flipped) OR reset ready.
pub fn usage_suggests_lifted(info: &UsageInfo, threshold: u8, post_limit: bool) -> bool {
    if post_limit {
        info.percentage < 95.0
    } else {
        info.percentage < f64::from(threshold)
    }
}

/// Parse the legacy org usage JSON.
fn parse_org_usage_json(json: &serde_json::Value) -> Option<UsageInfo> {
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

    // --- OAuth usage JSON parsing (Claude Code /usage shape) ---

    #[test]
    fn test_parse_oauth_usage_prefers_exhausted_session_reset() {
        // Mirrors a live Max-plan response: session at 100% with near reset,
        // weekly far in the future. The wait must target the session window.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 100.0,
                "resets_at": "2026-08-06T01:19:59.584282+00:00"
            },
            "seven_day": {
                "utilization": 11.0,
                "resets_at": "2026-08-12T19:59:59.584309+00:00"
            },
            "limits": [
                {
                    "kind": "session",
                    "percent": 100,
                    "severity": "critical",
                    "resets_at": "2026-08-06T01:19:59.584282+00:00",
                    "is_active": true
                },
                {
                    "kind": "weekly_all",
                    "percent": 11,
                    "severity": "normal",
                    "resets_at": "2026-08-12T19:59:59.584309+00:00",
                    "is_active": false
                }
            ]
        });
        let info = parse_oauth_usage_json(&json).expect("oauth json must parse");
        assert!((info.percentage - 100.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-08-06T01:19:59.584282+00:00"),
            "must pick the exhausted session reset, not the weekly one"
        );
    }

    #[test]
    fn test_parse_oauth_usage_fractional_utilization() {
        // Some clients report utilization as 0.0–1.0 fractions.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 0.42,
                "resets_at": "2026-02-28T17:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("fractional util must parse");
        assert!((info.percentage - 42.0).abs() < f64::EPSILON);
        assert_eq!(info.reset_at.as_deref(), Some("2026-02-28T17:00:00Z"));
    }

    #[test]
    fn test_parse_oauth_usage_empty_returns_none() {
        let json = serde_json::json!({ "extra_usage": { "utilization": 100.0 } });
        assert!(
            parse_oauth_usage_json(&json).is_none(),
            "extra_usage alone (no time-window) is not a waitable reset"
        );
    }

    #[test]
    fn test_parse_oauth_usage_weekly_exhausted_session_low() {
        // Weekly at 100%, session fine → percentage 100, reset = weekly.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 20.0,
                "resets_at": "2026-08-06T01:00:00Z"
            },
            "seven_day": {
                "utilization": 100.0,
                "resets_at": "2026-08-12T19:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!((info.percentage - 100.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-08-12T19:00:00Z"),
            "exhausted weekly must win over non-exhausted session"
        );
    }

    #[test]
    fn test_parse_oauth_usage_none_exhausted_prefers_five_hour() {
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 90.0,
                "resets_at": "2026-08-06T01:00:00Z"
            },
            "seven_day": {
                "utilization": 90.0,
                "resets_at": "2026-08-12T19:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!((info.percentage - 90.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-08-06T01:00:00Z"),
            "when nothing exhausted, prefer five_hour.resets_at"
        );
    }

    #[test]
    fn test_usage_suggests_lifted_post_limit() {
        let high = UsageInfo {
            percentage: 100.0,
            reset_at: None,
        };
        let low = UsageInfo {
            percentage: 40.0,
            reset_at: None,
        };
        assert!(!usage_suggests_lifted(&high, 80, true));
        assert!(usage_suggests_lifted(&low, 80, true));
        assert!(!usage_suggests_lifted(&high, 80, false));
        assert!(usage_suggests_lifted(&low, 80, false));
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

    // --- Usage API URL constants ---

    #[test]
    fn test_usage_api_urls_are_https() {
        assert!(
            OAUTH_USAGE_API_URL.starts_with("https://"),
            "OAuth usage API URL should use HTTPS"
        );
        assert!(
            ORG_USAGE_API_URL.starts_with("https://"),
            "Org usage API URL should use HTTPS"
        );
    }

    #[test]
    fn test_usage_api_urls_contain_anthropic() {
        assert!(
            OAUTH_USAGE_API_URL.contains("anthropic.com"),
            "OAuth usage API URL should point to anthropic.com"
        );
        assert!(
            ORG_USAGE_API_URL.contains("anthropic.com"),
            "Org usage API URL should point to anthropic.com"
        );
        assert!(
            OAUTH_USAGE_API_URL.contains("/api/oauth/usage"),
            "OAuth usage path must match Claude Code /usage HUD endpoint"
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
