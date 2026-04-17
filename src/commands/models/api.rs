//! Anthropic `/v1/models` discovery.
//!
//! Small HTTP client over the project's existing `ureq` dep. All errors fall
//! back silently at the ensure_default_model orchestrator layer — this module
//! just surfaces the error variants so the caller can choose how to react.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Anthropic Messages API `/v1/models` endpoint.
const MODELS_URL: &str = "https://api.anthropic.com/v1/models";

/// Anthropic API version header — pinned. Newer versions are backward compat
/// for this endpoint, so we don't need to track their latest.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A single model entry returned by the API.
///
/// Fields beyond `id` are optional to tolerate schema drift. `created_at`
/// may be absent or malformed; callers sort such entries last.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteModel {
    /// Stable model identifier (e.g. the value of
    /// `loop_engine::model::OPUS_MODEL`).
    pub id: String,
    /// Human-friendly name, e.g. `"Claude Opus 4.7"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Creation timestamp. Parsed via chrono; unparseable values become `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "parse_optional_timestamp"
    )]
    pub created_at: Option<DateTime<Utc>>,
    /// Object type, typically `"model"`. Kept for forward-compat diagnostics.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Top-level response shape.
///
/// The endpoint supports pagination (`has_more`, `first_id`, `last_id`) — v1
/// of this client grabs the first page only. The model list fits comfortably.
#[derive(Debug, Deserialize)]
struct ListResponse {
    data: Vec<RemoteModel>,
    #[serde(default)]
    #[allow(dead_code)]
    has_more: bool,
    #[serde(default)]
    #[allow(dead_code)]
    first_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    last_id: Option<String>,
}

/// Errors from the remote-fetch path.
///
/// `NoKey` and `NotOptedIn` short-circuit before any HTTP request is made.
/// `Transport` covers connect / DNS / timeout / read errors.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("ANTHROPIC_API_KEY not set")]
    NoKey,
    #[error("TASK_MGR_USE_API is not set to 1; remote model discovery disabled")]
    NotOptedIn,
    #[error("Anthropic API returned HTTP {0}")]
    Http(u16),
    #[error("failed to parse /v1/models response: {0}")]
    Parse(String),
    #[error("transport error contacting Anthropic: {0}")]
    Transport(String),
}

/// Parse a raw `/v1/models` JSON response into a list of models.
/// Pure function — unit-testable without the network.
pub fn parse_models_response(json: &str) -> Result<Vec<RemoteModel>, ApiError> {
    let response: ListResponse =
        serde_json::from_str(json).map_err(|e| ApiError::Parse(e.to_string()))?;
    Ok(response.data)
}

/// Sort by `created_at` descending (newest first). Entries with `None`
/// (missing/unparseable) sort to the end in stable order so they don't
/// poison a useful list.
pub fn sort_newest_first(models: &mut [RemoteModel]) {
    models.sort_by(|a, b| match (a.created_at, b.created_at) {
        // Both dated: newer first means we want descending on created_at,
        // which is `b.cmp(&a)` — Less when b > a means a goes later.
        (Some(a_ts), Some(b_ts)) => b_ts.cmp(&a_ts),
        // Dated entry before undated: keep undated at the end.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// Fetch and parse the live model list.
///
/// Returns `ApiError::NoKey` / `NotOptedIn` synchronously without making a
/// request if the preconditions aren't met — check these in the caller before
/// spending a roundtrip.
pub fn fetch_models() -> Result<Vec<RemoteModel>, ApiError> {
    check_opt_in()?;
    let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| ApiError::NoKey)?;
    fetch_models_with_key(&key)
}

/// Perform the HTTP call with an explicit key. Split out so tests can
/// inject a mock server URL via an env override in the future if needed.
fn fetch_models_with_key(api_key: &str) -> Result<Vec<RemoteModel>, ApiError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .build();

    let response = agent
        .get(MODELS_URL)
        .set("x-api-key", api_key)
        .set("anthropic-version", ANTHROPIC_VERSION)
        .set("Accept", "application/json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => ApiError::Http(code),
            ureq::Error::Transport(t) => ApiError::Transport(t.to_string()),
        })?;

    let body = response
        .into_string()
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    parse_models_response(&body)
}

/// Returns `Ok(())` iff `TASK_MGR_USE_API=1`. Users must explicitly opt in
/// so a globally-exported `ANTHROPIC_API_KEY` (for SDK work) doesn't cause
/// task-mgr to silently start making API calls.
pub fn check_opt_in() -> Result<(), ApiError> {
    if std::env::var("TASK_MGR_USE_API").as_deref() == Ok("1") {
        Ok(())
    } else {
        Err(ApiError::NotOptedIn)
    }
}

fn parse_optional_timestamp<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Accept either a string or null; unparseable strings become None.
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};

    #[test]
    fn parses_well_formed_response() {
        let json = format!(
            r#"{{
            "data": [
                {{"id": "{OPUS_MODEL}", "type": "model", "display_name": "Claude Opus", "created_at": "2026-03-15T00:00:00Z"}},
                {{"id": "{SONNET_MODEL}", "type": "model", "display_name": "Claude Sonnet", "created_at": "2025-11-01T00:00:00Z"}}
            ],
            "has_more": false,
            "first_id": "{OPUS_MODEL}",
            "last_id": "{SONNET_MODEL}"
        }}"#
        );
        let models = parse_models_response(&json).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, OPUS_MODEL);
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Opus"));
        assert!(models[0].created_at.is_some());
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let json = r#"{"data": [{"id": "future-model"}], "has_more": false}"#;
        let models = parse_models_response(json).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "future-model");
        assert!(models[0].display_name.is_none());
        assert!(models[0].created_at.is_none());
    }

    #[test]
    fn tolerates_unknown_top_level_fields() {
        let json = r#"{"data": [], "has_more": false, "futureField": "ok"}"#;
        assert!(parse_models_response(json).is_ok());
    }

    #[test]
    fn malformed_created_at_becomes_none() {
        let json = r#"{"data": [{"id": "x", "created_at": "not a timestamp"}], "has_more": false}"#;
        let models = parse_models_response(json).unwrap();
        assert!(models[0].created_at.is_none());
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let err = parse_models_response("totally not json").unwrap_err();
        assert!(matches!(err, ApiError::Parse(_)));
    }

    #[test]
    fn missing_data_field_returns_parse_error() {
        let err = parse_models_response(r#"{"has_more": false}"#).unwrap_err();
        assert!(matches!(err, ApiError::Parse(_)));
    }

    #[test]
    fn sort_newest_first_puts_newer_first() {
        let mk = |id: &str, ts: &str| RemoteModel {
            id: id.to_string(),
            display_name: None,
            created_at: DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|d| d.with_timezone(&Utc)),
            kind: None,
        };
        let mut models = vec![
            mk("b", "2025-01-01T00:00:00Z"),
            mk("a", "2026-06-01T00:00:00Z"),
            mk("c", "2024-01-01T00:00:00Z"),
        ];
        sort_newest_first(&mut models);
        assert_eq!(models[0].id, "a");
        assert_eq!(models[1].id, "b");
        assert_eq!(models[2].id, "c");
    }

    #[test]
    fn sort_puts_missing_created_at_last() {
        let with_ts = RemoteModel {
            id: "has-ts".to_string(),
            display_name: None,
            created_at: Some(Utc::now()),
            kind: None,
        };
        let without = RemoteModel {
            id: "no-ts".to_string(),
            display_name: None,
            created_at: None,
            kind: None,
        };
        let mut models = vec![without.clone(), with_ts.clone(), without.clone()];
        sort_newest_first(&mut models);
        assert_eq!(models[0].id, "has-ts");
        assert_eq!(models[1].id, "no-ts");
        assert_eq!(models[2].id, "no-ts");
    }
}
