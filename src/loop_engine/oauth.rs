//! OAuth token management for Claude API access.
//!
//! Reads credentials from `~/.claude/.credentials.json`, checks token expiry,
//! and refreshes via the Anthropic OAuth endpoint when needed. All operations
//! degrade gracefully if credentials are unavailable.
//!
//! **Security**: Tokens are never logged, printed, or included in prompts.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Anthropic OAuth token refresh endpoint.
const OAUTH_REFRESH_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Default buffer before expiry (in minutes) to trigger refresh.
const DEFAULT_EXPIRY_BUFFER_MINUTES: u64 = 5;

/// Credentials file content structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    /// The current access token (NEVER log this)
    pub access_token: String,
    /// The refresh token used to obtain new access tokens (NEVER log this)
    pub refresh_token: String,
    /// Expiry timestamp in milliseconds since epoch
    pub expires_at: u64,
}

/// Result of a token operation.
#[derive(Debug)]
pub enum TokenResult {
    /// Token is valid and ready to use
    Valid,
    /// Token was refreshed successfully
    Refreshed,
    /// No credentials file found (graceful degradation)
    NoCreds,
    /// Refresh failed (with sanitized error message)
    RefreshFailed(String),
}

/// Get the path to the credentials file.
pub fn credentials_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".claude")
        .join(".credentials.json")
}

/// Read credentials from the credentials file.
///
/// Returns `None` if the file doesn't exist or can't be parsed.
/// **Never** logs the file contents.
pub fn read_credentials(path: &PathBuf) -> Option<Credentials> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Check if the token expires within the given buffer (in minutes).
///
/// Returns `true` if the token is expired or will expire within `buffer_minutes`.
pub fn is_token_expiring(creds: &Credentials, buffer_minutes: u64) -> bool {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_millis() as u64;

    let buffer_ms = buffer_minutes * 60 * 1000;
    creds.expires_at <= now_ms + buffer_ms
}

/// Refresh the access token using the refresh token.
///
/// Makes a POST request to the Anthropic OAuth endpoint.
/// On success, atomically writes the updated credentials to the file.
///
/// Returns sanitized error message on failure (never includes tokens).
pub fn refresh_token(creds_path: &PathBuf, creds: &Credentials) -> Result<Credentials, String> {
    let response = ureq::post(OAUTH_REFRESH_URL)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &creds.refresh_token),
        ])
        .map_err(|e| sanitize_oauth_error(&e.to_string()))?;

    // Parse response
    let response_json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse OAuth response: {}", e))?;

    let access_token = response_json["access_token"]
        .as_str()
        .ok_or("Missing access_token in OAuth response")?
        .to_string();

    let expires_in = response_json["expires_in"].as_u64().unwrap_or(3600); // Default 1 hour

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_millis() as u64;

    let new_creds = Credentials {
        access_token,
        refresh_token: response_json["refresh_token"]
            .as_str()
            .unwrap_or(&creds.refresh_token)
            .to_string(),
        expires_at: now_ms + (expires_in * 1000),
    };

    // Atomic write
    write_credentials_atomic(creds_path, &new_creds)?;

    Ok(new_creds)
}

/// Ensure the access token is valid, refreshing if needed.
///
/// This is the main entry point for pre-iteration token management.
/// Degrades gracefully if credentials are unavailable.
pub fn ensure_valid_token() -> TokenResult {
    let path = credentials_path();

    let creds = match read_credentials(&path) {
        Some(c) => c,
        None => return TokenResult::NoCreds,
    };

    if !is_token_expiring(&creds, DEFAULT_EXPIRY_BUFFER_MINUTES) {
        return TokenResult::Valid;
    }

    eprintln!("OAuth token expiring, refreshing...");

    match refresh_token(&path, &creds) {
        Ok(_) => {
            eprintln!("OAuth token refreshed successfully");
            TokenResult::Refreshed
        }
        Err(e) => {
            eprintln!("Warning: OAuth token refresh failed: {}", e);
            TokenResult::RefreshFailed(e)
        }
    }
}

/// Write credentials atomically using tempfile + rename.
///
/// Sets file permissions to 600 (owner read/write only) on Unix.
fn write_credentials_atomic(path: &PathBuf, creds: &Credentials) -> Result<(), String> {
    let json = serde_json::to_string_pretty(creds)
        .map_err(|e| format!("Failed to serialize credentials: {}", e))?;

    let parent = path
        .parent()
        .ok_or("Credentials path has no parent directory")?;

    // Ensure parent directory exists
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create credentials directory: {}", e))?;

    // Write to temp file in the same directory (for atomic rename)
    let temp_path = parent.join(".credentials.json.tmp");
    fs::write(&temp_path, &json)
        .map_err(|e| format!("Failed to write temp credentials file: {}", e))?;

    // Set permissions to 600 on Unix
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&temp_path, perms)
            .map_err(|e| format!("Failed to set credentials file permissions: {}", e))?;
    }

    // Atomic rename
    fs::rename(&temp_path, path)
        .map_err(|e| format!("Failed to atomically update credentials file: {}", e))?;

    Ok(())
}

/// Sanitize OAuth error messages to ensure tokens are not leaked.
///
/// Delegates to the shared `sanitize_error_tokens` utility.
fn sanitize_oauth_error(error: &str) -> String {
    super::sanitize_error_tokens(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- Credentials parsing tests ---

    #[test]
    fn test_read_credentials_valid_json() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");

        let json = r#"{
            "accessToken": "test-token-123",
            "refreshToken": "refresh-token-456",
            "expiresAt": 1700000000000
        }"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        let creds = read_credentials(&path);
        assert!(creds.is_some());

        let c = creds.unwrap();
        assert_eq!(c.access_token, "test-token-123");
        assert_eq!(c.refresh_token, "refresh-token-456");
        assert_eq!(c.expires_at, 1700000000000);
    }

    #[test]
    fn test_read_credentials_missing_file() {
        let path = PathBuf::from("/nonexistent/creds.json");
        let creds = read_credentials(&path);
        assert!(creds.is_none());
    }

    #[test]
    fn test_read_credentials_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        fs::write(&creds_path, "not json").unwrap();

        let path = PathBuf::from(&creds_path);
        let creds = read_credentials(&path);
        assert!(creds.is_none());
    }

    // --- Token expiry tests ---

    #[test]
    fn test_is_token_expiring_expired() {
        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: 0, // Expired long ago
        };
        assert!(is_token_expiring(&creds, 5));
    }

    #[test]
    fn test_is_token_expiring_not_expired() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + 3_600_000, // 1 hour from now
        };
        assert!(!is_token_expiring(&creds, 5));
    }

    #[test]
    fn test_is_token_expiring_within_buffer() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + 120_000, // 2 minutes from now
        };
        // Buffer is 5 minutes, so this should be expiring
        assert!(is_token_expiring(&creds, 5));
    }

    #[test]
    fn test_is_token_expiring_zero_buffer() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + 1000, // 1 second from now
        };
        assert!(!is_token_expiring(&creds, 0));
    }

    // --- Atomic write tests ---

    #[test]
    fn test_write_credentials_atomic_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");

        let creds = Credentials {
            access_token: "new-token".to_string(),
            refresh_token: "new-refresh".to_string(),
            expires_at: 1700000000000,
        };

        let path = PathBuf::from(&creds_path);
        write_credentials_atomic(&path, &creds).unwrap();

        assert!(creds_path.exists());
        let content = fs::read_to_string(&creds_path).unwrap();
        assert!(content.contains("new-token"));
    }

    #[cfg(unix)]
    #[test]
    fn test_write_credentials_atomic_sets_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");

        let creds = Credentials {
            access_token: "token".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 1700000000000,
        };

        let path = PathBuf::from(&creds_path);
        write_credentials_atomic(&path, &creds).unwrap();

        let metadata = fs::metadata(&creds_path).unwrap();
        let mode = metadata.permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "File should have 600 permissions");
    }

    #[test]
    fn test_write_credentials_atomic_creates_parent_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("subdir").join("creds.json");

        let creds = Credentials {
            access_token: "token".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 1700000000000,
        };

        let path = PathBuf::from(&creds_path);
        write_credentials_atomic(&path, &creds).unwrap();
        assert!(creds_path.exists());
    }

    // --- Sanitize tests ---

    #[test]
    fn test_sanitize_oauth_error_redacts_long_tokens() {
        let error = "Error: invalid_grant abcdefghijklmnopqrstuvwxyz123456 is expired";
        let sanitized = sanitize_oauth_error(error);
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn test_sanitize_oauth_error_preserves_short_words() {
        let error = "Error: connection timeout after 30s";
        let sanitized = sanitize_oauth_error(error);
        assert_eq!(sanitized, "Error: connection timeout after 30s");
    }

    #[test]
    fn test_sanitize_oauth_error_empty_string() {
        assert_eq!(sanitize_oauth_error(""), "");
    }

    // --- TokenResult tests ---

    #[test]
    fn test_token_result_variants() {
        // Verify all variants can be constructed
        let _ = TokenResult::Valid;
        let _ = TokenResult::Refreshed;
        let _ = TokenResult::NoCreds;
        let _ = TokenResult::RefreshFailed("test error".to_string());
    }

    // --- credentials_path test ---

    #[test]
    fn test_credentials_path_contains_claude() {
        let path = credentials_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains(".claude"),
            "Path should contain .claude directory"
        );
        assert!(
            path_str.contains(".credentials.json"),
            "Path should end with .credentials.json"
        );
    }

    // --- Credentials serialization ---

    #[test]
    fn test_credentials_round_trip() {
        let creds = Credentials {
            access_token: "token123".to_string(),
            refresh_token: "refresh456".to_string(),
            expires_at: 1700000000000,
        };

        let json = serde_json::to_string(&creds).unwrap();
        let parsed: Credentials = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.access_token, "token123");
        assert_eq!(parsed.refresh_token, "refresh456");
        assert_eq!(parsed.expires_at, 1700000000000);
    }

    // ========================================================
    // Comprehensive edge case tests (TEST-004)
    // ========================================================

    // --- Malformed credentials JSON ---

    #[test]
    fn test_read_credentials_missing_access_token() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{"refreshToken": "refresh", "expiresAt": 1700000000000}"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "Missing accessToken should fail to parse"
        );
    }

    #[test]
    fn test_read_credentials_missing_refresh_token() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{"accessToken": "token", "expiresAt": 1700000000000}"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "Missing refreshToken should fail to parse"
        );
    }

    #[test]
    fn test_read_credentials_missing_expires_at() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{"accessToken": "token", "refreshToken": "refresh"}"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "Missing expiresAt should fail to parse"
        );
    }

    #[test]
    fn test_read_credentials_truncated_json() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{"accessToken": "tok"#; // truncated
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "Truncated JSON should fail to parse"
        );
    }

    #[test]
    fn test_read_credentials_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        fs::write(&creds_path, "").unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "Empty file should fail to parse"
        );
    }

    #[test]
    fn test_read_credentials_json_array_instead_of_object() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        fs::write(&creds_path, "[1, 2, 3]").unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "JSON array should fail to parse as Credentials"
        );
    }

    #[test]
    fn test_read_credentials_wrong_types() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        // expiresAt as string instead of number
        let json =
            r#"{"accessToken": "token", "refreshToken": "refresh", "expiresAt": "not-a-number"}"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        assert!(
            read_credentials(&path).is_none(),
            "Wrong type for expiresAt should fail"
        );
    }

    #[test]
    fn test_read_credentials_extra_fields_forward_compat() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{
            "accessToken": "token",
            "refreshToken": "refresh",
            "expiresAt": 1700000000000,
            "extraField": "should be ignored",
            "anotherField": 42
        }"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        let creds = read_credentials(&path);
        assert!(
            creds.is_some(),
            "Extra fields should be ignored (forward compat)"
        );
        let c = creds.unwrap();
        assert_eq!(c.access_token, "token");
    }

    // --- Token expiry boundary tests ---

    #[test]
    fn test_is_token_expiring_exact_boundary() {
        // Token expires exactly at now + buffer
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let buffer_minutes = 5u64;
        let buffer_ms = buffer_minutes * 60 * 1000;
        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + buffer_ms, // Exactly at boundary
        };
        // At exact boundary: expires_at <= now_ms + buffer_ms evaluates to true
        assert!(
            is_token_expiring(&creds, buffer_minutes),
            "Token at exact boundary should be considered expiring"
        );
    }

    #[test]
    fn test_is_token_expiring_one_ms_past_boundary() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let buffer_minutes = 5u64;
        let buffer_ms = buffer_minutes * 60 * 1000;
        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + buffer_ms + 1000, // 1 second past boundary
        };
        assert!(
            !is_token_expiring(&creds, buffer_minutes),
            "Token 1s past boundary should NOT be expiring"
        );
    }

    #[test]
    fn test_is_token_expiring_large_buffer() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + 86_400_000, // 24 hours from now
        };
        // Buffer of 1440 minutes (24 hours) should make it expiring
        assert!(is_token_expiring(&creds, 1440));
    }

    #[test]
    fn test_is_token_expiring_max_expires_at() {
        // Token with very far future expiry
        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: u64::MAX,
        };
        assert!(
            !is_token_expiring(&creds, 5),
            "u64::MAX expiry should never be expiring"
        );
    }

    #[test]
    fn test_is_token_expiring_buffer_overflow_protection() {
        // Large buffer that could overflow when multiplied
        // buffer_minutes * 60 * 1000 with buffer_minutes = u64::MAX / 60000
        // should not panic due to overflow
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let creds = Credentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now_ms + 1_000_000,
        };
        // Very large buffer — arithmetic overflow wraps silently (u64)
        // buffer_ms = u64::MAX/60001 * 60 * 1000 may wrap; the function uses
        // plain multiplication, so it will wrap. This is a known limitation
        // but not a safety issue (it just means "expiring" which is the safe default).
        let large_buffer = u64::MAX / 60_001;
        let result = is_token_expiring(&creds, large_buffer);
        // We just verify it doesn't panic
        let _ = result;
    }

    // --- Atomic write error handling ---

    #[test]
    fn test_write_credentials_atomic_overwrites_existing() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");

        let first = Credentials {
            access_token: "first-token".to_string(),
            refresh_token: "first-refresh".to_string(),
            expires_at: 1000,
        };
        let second = Credentials {
            access_token: "second-token".to_string(),
            refresh_token: "second-refresh".to_string(),
            expires_at: 2000,
        };

        let path = PathBuf::from(&creds_path);
        write_credentials_atomic(&path, &first).unwrap();
        write_credentials_atomic(&path, &second).unwrap();

        let content = fs::read_to_string(&creds_path).unwrap();
        assert!(
            content.contains("second-token"),
            "Should contain second write"
        );
        assert!(
            !content.contains("first-token"),
            "Should not contain first write"
        );
    }

    #[test]
    fn test_write_credentials_atomic_deeply_nested_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("creds.json");

        let creds = Credentials {
            access_token: "nested".to_string(),
            refresh_token: "deep".to_string(),
            expires_at: 3000,
        };

        let path = PathBuf::from(&creds_path);
        write_credentials_atomic(&path, &creds).unwrap();
        assert!(creds_path.exists());
    }

    #[test]
    fn test_write_and_read_roundtrip_via_file() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");

        let original = Credentials {
            access_token: "round-trip-token".to_string(),
            refresh_token: "round-trip-refresh".to_string(),
            expires_at: 9999999999999,
        };

        let path = PathBuf::from(&creds_path);
        write_credentials_atomic(&path, &original).unwrap();

        let loaded = read_credentials(&path).expect("Should read back written credentials");
        assert_eq!(loaded.access_token, "round-trip-token");
        assert_eq!(loaded.refresh_token, "round-trip-refresh");
        assert_eq!(loaded.expires_at, 9999999999999);
    }

    // --- Sanitize edge cases ---

    #[test]
    fn test_sanitize_oauth_error_multiple_tokens() {
        let error =
            "Error: token1_abcdefghijklmnopqrstuvwxyz and token2_abcdefghijklmnopqrstuvwxyz failed";
        let sanitized = sanitize_oauth_error(error);
        // Both long tokens should be redacted
        assert!(!sanitized.contains("token1_abcdefghijklmnopqrstuvwxyz"));
        assert!(!sanitized.contains("token2_abcdefghijklmnopqrstuvwxyz"));
        assert!(sanitized.contains("[REDACTED]"));
        // Short words preserved
        assert!(sanitized.contains("Error:"));
        assert!(sanitized.contains("and"));
        assert!(sanitized.contains("failed"));
    }

    #[test]
    fn test_sanitize_oauth_error_exactly_20_chars_not_redacted() {
        // 20 chars exactly — threshold is >20, so 20 chars should NOT be redacted
        let token = "abcdefghijklmnopqrst"; // 20 chars
        assert_eq!(token.len(), 20);
        let sanitized = sanitize_oauth_error(token);
        assert_eq!(
            sanitized, token,
            "Exactly 20 char token should not be redacted"
        );
    }

    #[test]
    fn test_sanitize_oauth_error_21_chars_redacted() {
        // 21 chars — should be redacted
        let token = "abcdefghijklmnopqrstu"; // 21 chars
        assert_eq!(token.len(), 21);
        let sanitized = sanitize_oauth_error(token);
        assert_eq!(sanitized, "[REDACTED]");
    }

    #[test]
    fn test_sanitize_oauth_error_with_special_chars_in_token() {
        // Token-like string with special chars — should NOT be redacted
        // because sanitize checks c.is_alphanumeric() || c == '-' || c == '_'
        let error = "Error: token.with.dots.longer.than.twenty.chars is bad";
        let sanitized = sanitize_oauth_error(error);
        // Dots are not alphanumeric/dash/underscore, so the long string stays
        assert!(sanitized.contains("token.with.dots.longer.than.twenty.chars"));
    }

    #[test]
    fn test_sanitize_oauth_error_whitespace_only() {
        let sanitized = sanitize_oauth_error("   ");
        assert_eq!(sanitized, "");
    }

    // --- Credentials path tests ---

    #[test]
    fn test_credentials_path_absolute() {
        let path = credentials_path();
        assert!(
            path.is_absolute(),
            "Credentials path should be absolute: {:?}",
            path
        );
    }

    #[test]
    fn test_credentials_path_ends_with_credentials_json() {
        let path = credentials_path();
        assert_eq!(path.file_name().unwrap(), ".credentials.json");
    }

    // --- Credentials with empty strings ---

    #[test]
    fn test_credentials_with_empty_tokens() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{"accessToken": "", "refreshToken": "", "expiresAt": 0}"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        let creds = read_credentials(&path);
        assert!(creds.is_some(), "Empty token strings should still parse");
        let c = creds.unwrap();
        assert_eq!(c.access_token, "");
        assert_eq!(c.refresh_token, "");
        assert_eq!(c.expires_at, 0);
    }

    #[test]
    fn test_credentials_with_unicode_tokens() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path = temp_dir.path().join("creds.json");
        let json = r#"{"accessToken": "tökén-日本語", "refreshToken": "rëfrësh-中文", "expiresAt": 1700000000000}"#;
        fs::write(&creds_path, json).unwrap();

        let path = PathBuf::from(&creds_path);
        let creds = read_credentials(&path).expect("Unicode tokens should parse");
        assert_eq!(creds.access_token, "tökén-日本語");
        assert_eq!(creds.refresh_token, "rëfrësh-中文");
    }

    // --- Serialization camelCase ---

    #[test]
    fn test_credentials_serialization_uses_camel_case() {
        let creds = Credentials {
            access_token: "t".to_string(),
            refresh_token: "r".to_string(),
            expires_at: 1,
        };
        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("accessToken"), "Should use camelCase");
        assert!(json.contains("refreshToken"), "Should use camelCase");
        assert!(json.contains("expiresAt"), "Should use camelCase");
        assert!(!json.contains("access_token"), "Should NOT use snake_case");
    }

    // --- TokenResult display ---

    #[test]
    fn test_token_result_debug_format() {
        let valid = TokenResult::Valid;
        let debug = format!("{:?}", valid);
        assert_eq!(debug, "Valid");

        let failed = TokenResult::RefreshFailed("timeout".to_string());
        let debug = format!("{:?}", failed);
        assert!(debug.contains("timeout"));
    }
}
