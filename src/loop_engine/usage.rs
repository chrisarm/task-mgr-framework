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

use crate::loop_engine::model::{CapabilityTier, Provider, ResolvedModelsConfig};
use crate::loop_engine::quota::{Measurement, MeasurementUnit, QuotaBucket};

/// Claude Code OAuth usage endpoint (matches `/usage` HUD).
const OAUTH_USAGE_API_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Beta header required by the OAuth usage endpoint.
const OAUTH_USAGE_BETA: &str = "oauth-2025-04-20";

/// Fallback UA when `claude --version` cannot be read.
///
/// The OAuth usage endpoint rate-limits callers that omit a `claude-code/`
/// prefix into an aggressive 429 bucket, so the 30s early-lift probe never
/// succeeds. Keep the family even when the version probe fails.
const OAUTH_USAGE_USER_AGENT_FALLBACK: &str = "claude-code/unknown";

/// Legacy org-level usage endpoint (API-key / org accounts).
const ORG_USAGE_API_URL: &str = "https://api.anthropic.com/v1/organizations/usage";

/// Default used-percent threshold for selecting `reset_at` among account-binding
/// windows. Matches `LoopConfig::usage_threshold` default (92). Callers that
/// know the live config (`check_and_wait`, post-output load) pass
/// `LoopConfig::usage_threshold` so wait duration tracks the same bar as the
/// percentage compare.
const DEFAULT_USAGE_THRESHOLD: f64 = 92.0;

/// Connect + response budget for usage GETs. Without this, a SYN hang to
/// Anthropic (or a wedged path when `~/.claude` credentials exist during unit
/// tests) can block a wave reaction for tens of minutes.
const USAGE_API_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const USAGE_API_RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn usage_http_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(USAGE_API_CONNECT_TIMEOUT))
            .timeout_recv_response(Some(USAGE_API_RECV_TIMEOUT))
            .timeout_recv_body(Some(USAGE_API_RECV_TIMEOUT))
            .build()
            .into()
    })
}

/// Usage information returned from the API.
#[derive(Debug, Clone)]
pub struct UsageInfo {
    /// Current usage as a percentage (0.0 - 100.0) of **used** quota.
    ///
    /// For the OAuth endpoint: **max used** across account-binding windows only
    /// (named `five_hour` / `seven_day`, plus `limits[]` with `kind` `session`
    /// or `weekly_all`). Rung-scoped windows (`seven_day_opus` /
    /// `seven_day_sonnet`, `limits[].kind = weekly_scoped`) are omitted so a
    /// frontier-only bucket cannot park the account gate.
    pub percentage: f64,
    /// ISO 8601 reset timestamp for waiting, if available.
    ///
    /// For the OAuth endpoint: **latest** `resets_at` among account-binding
    /// windows whose used percent is ≥ the live gate threshold (default
    /// [`DEFAULT_USAGE_THRESHOLD`] / 92, or `LoopConfig::usage_threshold` when
    /// threaded through [`load_usage_info_with_threshold`]); if none are
    /// gate-relevant, prefer the session window (`five_hour` or `limits[]`
    /// kind `session`), else any account-binding reset. Not the soonest
    /// exhausted / severity-critical timestamp across all windows.
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
    check_usage_api_with_threshold(access_token, DEFAULT_USAGE_THRESHOLD as u8)
}

/// Like [`check_usage_api`], but `reset_at` uses `threshold` as the
/// gate-relevant bar (same value `check_and_wait` compares against
/// `percentage`).
pub fn check_usage_api_with_threshold(access_token: &str, threshold: u8) -> Option<UsageInfo> {
    match fetch_oauth_usage(access_token, threshold) {
        Some(info) => Some(info),
        None => fetch_org_usage(access_token),
    }
}

/// Fetch Claude Code OAuth usage (five_hour / seven_day / limits[]).
fn fetch_oauth_usage(access_token: &str, threshold: u8) -> Option<UsageInfo> {
    let mut response = match usage_http_agent()
        .get(OAUTH_USAGE_API_URL)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("anthropic-beta", OAUTH_USAGE_BETA)
        .header("User-Agent", oauth_usage_user_agent())
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

    parse_oauth_usage_json_with_threshold(&json, f64::from(threshold))
}

/// User-Agent for `GET /api/oauth/usage`.
///
/// Reads the installed Claude Code version once per process (`$CLAUDE_BINARY
/// --version`, default `claude`) so a CLI upgrade is picked up on the next
/// loop start without a hardcoded pin. Cached: the 30s early-lift probe must
/// not spawn a process on every tick.
fn oauth_usage_user_agent() -> &'static str {
    static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHED.get_or_init(detect_claude_code_user_agent)
}

fn detect_claude_code_user_agent() -> String {
    match query_claude_cli_version() {
        Some(ver) => format!("claude-code/{ver}"),
        None => OAUTH_USAGE_USER_AGENT_FALLBACK.to_string(),
    }
}

fn query_claude_cli_version() -> Option<String> {
    let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());
    let output = std::process::Command::new(&binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_claude_version_output(&String::from_utf8_lossy(&output.stdout))
}

/// Pull a `x.y` / `x.y.z` version from `claude --version` stdout.
///
/// Live CLI prints `2.1.257 (Claude Code)`. First semver-like token wins.
pub(crate) fn parse_claude_version_output(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?.trim();
    for tok in line.split_whitespace() {
        let tok = tok.trim_matches(|c: char| !(c.is_ascii_digit() || c == '.'));
        if is_dotted_version(tok) {
            return Some(tok.to_string());
        }
    }
    None
}

fn is_dotted_version(s: &str) -> bool {
    !s.is_empty()
        && s.contains('.')
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Fetch legacy org-level usage endpoint.
fn fetch_org_usage(access_token: &str) -> Option<UsageInfo> {
    let mut response = match usage_http_agent()
        .get(ORG_USAGE_API_URL)
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

/// One account-binding utilization window from the OAuth usage payload.
struct UsageWindow {
    util: f64,
    reset: Option<String>,
    /// Named `five_hour` or `limits[]` kind `session` — preferred when no
    /// account-binding window is ≥ the live gate threshold.
    is_session: bool,
}

/// Ingest every OAuth usage object sibling and `limits[]` row into generic
/// [`QuotaBucket`]s (PR-2 / FR-003).
///
/// Walks **all** object siblings with `utilization` or `dollars` — no
/// window-name allow-list. Null / non-object siblings are skipped. Malformed
/// rows (no usable measurement) are skipped without panicking.
///
/// Rung mapping (Claude OAuth HUD only):
/// 1. `scope.model.display_name` / `id` via HUD label table (case-insensitive
///    prefix/token): Fable→frontier, Opus→standard, Sonnet→cost-efficient,
///    Haiku→cheapest.
/// 2. Else unlabeled ids: family token as substring of a *defined* configured
///    model string (`exact_model_for`, no clamp).
/// 3. After a HUD map to rung R, extra-mark every defined rung whose configured
///    model string equals R's (string equality — not substring `tier_of`).
///
/// Does **not** change [`parse_oauth_usage_json_with_threshold`] (PR-1 fold stays
/// account-binding / threshold-only until FEAT-004).
pub fn ingest_oauth_value(
    json: &serde_json::Value,
    models: &ResolvedModelsConfig,
) -> Vec<QuotaBucket> {
    let mut out = Vec::new();

    if let Some(obj) = json.as_object() {
        for (key, value) in obj {
            if key == "limits" {
                continue;
            }
            if value.is_null() || !value.is_object() {
                continue;
            }
            if let Some(bucket) = ingest_named_sibling(key, value, models) {
                out.push(bucket);
            }
        }
    }

    if let Some(limits) = json.get("limits").and_then(|v| v.as_array()) {
        for (idx, limit) in limits.iter().enumerate() {
            if limit.is_null() || !limit.is_object() {
                continue;
            }
            if let Some(bucket) = ingest_limits_row(idx, limit, models) {
                out.push(bucket);
            }
        }
    }

    out
}

fn ingest_named_sibling(
    key: &str,
    value: &serde_json::Value,
    models: &ResolvedModelsConfig,
) -> Option<QuotaBucket> {
    let measurements = measurements_from_object(value)?;
    let kind = kind_for_named_key(key);
    let resets_at = value
        .get("resets_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let severity = value
        .get("severity")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let is_active = value.get("is_active").and_then(|v| v.as_bool());

    // Named siblings rarely carry scope.model; map via id family token when
    // the kind is rung-scoped (seven_day_*), else leave account-binding.
    let rungs = if kind == "weekly_scoped" || looks_rung_scoped_key(key) {
        let token = family_token_from_id(key);
        let mapped = map_unlabeled_token(models, Provider::Claude, &token);
        if mapped.is_empty() {
            None
        } else {
            Some(mapped)
        }
    } else {
        None
    };

    Some(QuotaBucket {
        id: key.to_string(),
        kind,
        label: String::new(),
        measurements,
        resets_at,
        severity,
        is_active,
        rungs,
    })
}

fn ingest_limits_row(
    idx: usize,
    limit: &serde_json::Value,
    models: &ResolvedModelsConfig,
) -> Option<QuotaBucket> {
    let kind = limit
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let measurements = measurements_from_limit(limit)?;
    let resets_at = limit
        .get("resets_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let severity = limit
        .get("severity")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let is_active = limit.get("is_active").and_then(|v| v.as_bool());

    let model_obj = limit.get("scope").and_then(|s| s.get("model"));
    let display_name = model_obj
        .and_then(|m| m.get("display_name"))
        .and_then(|v| v.as_str());
    let model_id = model_obj.and_then(|m| m.get("id")).and_then(|v| v.as_str());

    let label = display_name.unwrap_or("").to_string();
    let id = if kind.is_empty() {
        format!("limits[{idx}]")
    } else {
        format!("limits[{idx}].{kind}")
    };

    let rungs = map_scope_to_rungs(models, Provider::Claude, display_name, model_id);

    Some(QuotaBucket {
        id,
        kind: if kind.is_empty() {
            "unknown".into()
        } else {
            kind
        },
        label,
        measurements,
        resets_at,
        severity,
        is_active,
        rungs,
    })
}

fn measurements_from_object(value: &serde_json::Value) -> Option<Vec<Measurement>> {
    let mut out = Vec::new();
    if let Some(util) = value.get("utilization").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: (100.0 - util).clamp(0.0, 100.0),
            unit: MeasurementUnit::Percent,
        });
    }
    if let Some(dollars) = value.get("dollars").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: dollars,
            unit: MeasurementUnit::Dollars,
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn measurements_from_limit(limit: &serde_json::Value) -> Option<Vec<Measurement>> {
    let mut out = Vec::new();
    if let Some(percent) = limit.get("percent").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: (100.0 - percent).clamp(0.0, 100.0),
            unit: MeasurementUnit::Percent,
        });
    } else if let Some(util) = limit.get("utilization").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: (100.0 - util).clamp(0.0, 100.0),
            unit: MeasurementUnit::Percent,
        });
    }
    if let Some(dollars) = limit.get("dollars").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: dollars,
            unit: MeasurementUnit::Dollars,
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn json_number_as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_u64().map(|u| u as f64))
        .or_else(|| v.as_i64().map(|i| i as f64))
}

fn kind_for_named_key(key: &str) -> String {
    match key {
        "five_hour" => "session".into(),
        "seven_day" => "weekly_all".into(),
        _ if looks_rung_scoped_key(key) => "weekly_scoped".into(),
        _ => key.to_string(),
    }
}

fn looks_rung_scoped_key(key: &str) -> bool {
    key.starts_with("seven_day_")
}

fn family_token_from_id(id: &str) -> String {
    id.rsplit('_').next().unwrap_or(id).to_ascii_lowercase()
}

/// HUD label table: case-insensitive prefix/token → capability tier.
/// Tokens live only in this ingest adapter — never in `quota.rs`.
fn hud_tier_from_label(label: &str) -> Option<CapabilityTier> {
    let lower = label.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let hit = |needle: &str| tokens.contains(&needle) || lower.starts_with(needle);
    if hit("fable") {
        Some(CapabilityTier::Frontier)
    } else if hit("opus") {
        Some(CapabilityTier::Standard)
    } else if hit("sonnet") {
        Some(CapabilityTier::CostEfficient)
    } else if hit("haiku") {
        Some(CapabilityTier::Cheapest)
    } else {
        None
    }
}

fn map_scope_to_rungs(
    models: &ResolvedModelsConfig,
    provider: Provider,
    display_name: Option<&str>,
    model_id: Option<&str>,
) -> Option<Vec<(Provider, CapabilityTier)>> {
    if let Some(name) = display_name
        && let Some(tier) = hud_tier_from_label(name)
    {
        return Some(extra_mark_rungs(models, provider, tier));
    }
    if let Some(id) = model_id {
        if let Some(tier) = hud_tier_from_label(id) {
            return Some(extra_mark_rungs(models, provider, tier));
        }
        // Unlabeled id: family-token substring against configured model strings.
        let token = family_token_from_id(id);
        let mapped = map_unlabeled_token(models, provider, &token);
        if !mapped.is_empty() {
            return Some(mapped);
        }
    }
    None
}

/// After HUD maps to rung R, also mark every defined rung whose configured
/// model string equals R's (exact string equality, no clamp / no tier_of).
fn extra_mark_rungs(
    models: &ResolvedModelsConfig,
    provider: Provider,
    primary: CapabilityTier,
) -> Vec<(Provider, CapabilityTier)> {
    let mut out = vec![(provider, primary)];
    let Some(primary_model) = models.exact_model_for(provider, primary) else {
        return out;
    };
    for tier in CapabilityTier::ALL {
        if tier == primary {
            continue;
        }
        if models.exact_model_for(provider, tier) == Some(primary_model) {
            out.push((provider, tier));
        }
    }
    out.sort_by_key(|(_, t)| *t);
    out.dedup();
    out
}

fn map_unlabeled_token(
    models: &ResolvedModelsConfig,
    provider: Provider,
    token: &str,
) -> Vec<(Provider, CapabilityTier)> {
    if token.is_empty() {
        return Vec::new();
    }
    let token_l = token.to_ascii_lowercase();
    let mut out = Vec::new();
    for tier in CapabilityTier::ALL {
        if let Some(model) = models.exact_model_for(provider, tier)
            && model.to_ascii_lowercase().contains(&token_l)
        {
            out.push((provider, tier));
        }
    }
    out.sort_by_key(|(_, t)| *t);
    out.dedup();
    out
}

/// Parse the Claude Code OAuth usage JSON into [`UsageInfo`] using the default
/// gate threshold ([`DEFAULT_USAGE_THRESHOLD`] / 92).
///
/// Test convenience wrapper. Production always calls
/// [`parse_oauth_usage_json_with_threshold`] with the live
/// `LoopConfig::usage_threshold` so `reset_at` matches the percentage compare
/// in `check_and_wait`.
#[cfg(test)]
pub(crate) fn parse_oauth_usage_json(json: &serde_json::Value) -> Option<UsageInfo> {
    parse_oauth_usage_json_with_threshold(json, DEFAULT_USAGE_THRESHOLD)
}

/// Parse the Claude Code OAuth usage JSON into [`UsageInfo`].
///
/// Account-binding fold (PR-1 / FR-001):
/// - **percentage** = max **used** (0–100) across account-binding windows only:
///   named `five_hour` / `seven_day`, plus `limits[]` with `kind` `session` or
///   `weekly_all`. Named `seven_day_opus` / `seven_day_sonnet` and
///   `limits[]` kinds `weekly_scoped` / `extra_usage` / `promotional` are
///   skipped. `severity` / `is_active` are display hints and do not enter the
///   fold.
/// - **reset_at** = latest among those windows with used ≥ `gate_threshold`
///   (live `LoopConfig::usage_threshold`, default 92); if none, prefer
///   session; else any account-binding reset. Gate-relevant means used ≥
///   threshold — not util ≥ 100 or `severity=critical`.
///
/// Pure / unit-testable — no I/O.
pub(crate) fn parse_oauth_usage_json_with_threshold(
    json: &serde_json::Value,
    gate_threshold: f64,
) -> Option<UsageInfo> {
    let mut windows: Vec<UsageWindow> = Vec::new();

    // Account-binding named buckets only (not seven_day_opus / seven_day_sonnet).
    for key in ["five_hour", "seven_day"] {
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
                is_session: key == "five_hour",
            });
        }
    }

    // Structured limits: only session / weekly_all (skip weekly_scoped etc.).
    if let Some(limits) = json.get("limits").and_then(|v| v.as_array()) {
        for limit in limits {
            let kind = limit.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind != "session" && kind != "weekly_all" {
                continue;
            }
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
            windows.push(UsageWindow {
                util: p,
                reset,
                is_session: kind == "session",
            });
        }
    }

    if windows.is_empty() {
        tracing::warn!("oauth usage API response missing utilization windows");
        return None;
    }

    let percentage = windows.iter().map(|w| w.util).fold(0.0_f64, f64::max);

    let reset_at = latest_reset(
        windows
            .iter()
            .filter(|w| w.util >= gate_threshold)
            .map(|w| &w.reset),
    )
    .or_else(|| {
        windows
            .iter()
            .find(|w| w.is_session)
            .and_then(|w| w.reset.clone())
    })
    .or_else(|| soonest_reset(windows.iter().map(|w| &w.reset)));

    Some(UsageInfo {
        percentage,
        reset_at,
    })
}

fn bucket_utilization(bucket: &serde_json::Value) -> Option<f64> {
    // Live OAuth `/api/oauth/usage` reports utilization as a 0–100 percentage
    // (`1.0` is one percent, not exhausted). Do not scale values ≤ 1.0: that
    // heuristic parks the loop at 100% after the first ~1% of a new window.
    bucket.get("utilization")?.as_f64()
}

fn parse_reset_timestamp(reset: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(reset)
        .ok()
        .map(|dt| dt.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(reset, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc().timestamp())
        })
}

/// Pick the chronologically soonest ISO-8601 reset timestamp from an iterator
/// of optional strings. Invalid / unparseable timestamps are skipped.
fn soonest_reset<'a>(resets: impl Iterator<Item = &'a Option<String>>) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for reset in resets.flatten() {
        if let Some(ts) = parse_reset_timestamp(reset) {
            match &best {
                Some((best_ts, _)) if ts >= *best_ts => {}
                _ => best = Some((ts, reset.clone())),
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Pick the chronologically latest ISO-8601 reset timestamp (for gate-relevant
/// account-binding windows). Invalid / unparseable timestamps are skipped.
fn latest_reset<'a>(resets: impl Iterator<Item = &'a Option<String>>) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for reset in resets.flatten() {
        if let Some(ts) = parse_reset_timestamp(reset) {
            match &best {
                Some((best_ts, _)) if ts <= *best_ts => {}
                _ => best = Some((ts, reset.clone())),
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Single chokepoint: credentials path → read → optional refresh → usage API.
///
/// Uses the default gate threshold (92) for `reset_at` selection. Prefer
/// [`load_usage_info_with_threshold`] when the live `usage_threshold` is known.
///
/// Used by the pre-iteration gate, post-rate-limit resolve, spillover blackout
/// duration, and early-lift probes. Returns `None` when credentials are missing
/// or both usage endpoints fail.
pub fn load_usage_info() -> Option<UsageInfo> {
    load_usage_info_with_threshold(DEFAULT_USAGE_THRESHOLD as u8)
}

/// Like [`load_usage_info`], but `reset_at` is selected with `threshold` as the
/// gate-relevant bar (same value compared to `percentage` in `check_and_wait`).
pub fn load_usage_info_with_threshold(threshold: u8) -> Option<UsageInfo> {
    let path = super::oauth::credentials_path();
    let mut creds = super::oauth::read_credentials(&path)?;
    if super::oauth::is_token_expiring(&creds, 5) {
        match super::oauth::refresh_token(&path, &creds) {
            Ok(refreshed) => {
                crate::output::ui::emit_err("OAuth token refreshed for usage check");
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
    check_usage_api_with_threshold(&creds.access_token, threshold)
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
    fn test_parse_oauth_usage_one_percent_is_not_exhausted() {
        // Live API: utilization 1.0 means 1%, not 100%. Scaling ≤1.0 as a
        // fraction parked loops after the first small task of a new window.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 1.0,
                "resets_at": "2026-09-01T23:00:00Z"
            },
            "seven_day": {
                "utilization": 11.0,
                "resets_at": "2026-09-05T19:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("oauth json must parse");
        assert!(
            (info.percentage - 11.0).abs() < f64::EPSILON,
            "max window is weekly 11%, not five_hour 1.0 scaled to 100; got {}",
            info.percentage
        );
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-01T23:00:00Z"),
            "nothing exhausted → prefer five_hour.resets_at"
        );
    }

    #[test]
    fn test_parse_oauth_usage_sonnet_one_percent_does_not_park() {
        // Live sample: seven_day_sonnet appears at 1.0 after the first Sonnet
        // task of a weekly window. Must not become the max-100 gate.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 33.0,
                "resets_at": "2026-04-11T07:00:00Z"
            },
            "seven_day": {
                "utilization": 13.0,
                "resets_at": "2026-04-17T00:59:59Z"
            },
            "seven_day_sonnet": {
                "utilization": 1.0,
                "resets_at": "2026-04-16T03:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("oauth json must parse");
        assert!((info.percentage - 33.0).abs() < f64::EPSILON);
        assert_eq!(info.reset_at.as_deref(), Some("2026-04-11T07:00:00Z"));
    }

    #[test]
    fn test_parse_oauth_usage_sub_one_percent_stays_sub_one() {
        // OAuth reports 0–100. A true 0.42% must stay below the 92% threshold,
        // not be scaled to 42%.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 0.42,
                "resets_at": "2026-02-28T17:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("sub-one percent must parse");
        assert!((info.percentage - 0.42).abs() < f64::EPSILON);
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
    fn test_parse_oauth_usage_live_fixture_ignores_scoped_and_named_rungs() {
        // Production-shaped HUD: session 24%, weekly-all 55%, Fable weekly_scoped
        // 95% critical + named opus/sonnet 100. Account fold must be max(24,55)=55
        // with session reset — 55 < default threshold 92 ⇒ BelowThreshold.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 24.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 55.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "seven_day_opus": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "seven_day_sonnet": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "limits": [
                {
                    "kind": "session",
                    "percent": 24,
                    "severity": "normal",
                    "resets_at": "2026-09-07T06:00:00Z",
                    "is_active": true
                },
                {
                    "kind": "weekly_all",
                    "percent": 55,
                    "severity": "normal",
                    "resets_at": "2026-09-12T19:00:00Z",
                    "is_active": false
                },
                {
                    "kind": "weekly_scoped",
                    "percent": 95,
                    "severity": "critical",
                    "is_active": true,
                    "resets_at": "2026-09-12T19:00:00Z",
                    "scope": {
                        "model": { "display_name": "Fable" }
                    }
                }
            ]
        });
        let info = parse_oauth_usage_json(&json).expect("live fixture must parse");
        assert!(
            (info.percentage - 55.0).abs() < f64::EPSILON,
            "account-binding max used is weekly-all 55, not Fable 95; got {}",
            info.percentage
        );
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-07T06:00:00Z"),
            "nothing ≥ 92 → prefer session reset, not weekly Fable"
        );
        assert!(
            info.percentage < DEFAULT_USAGE_THRESHOLD,
            "55 < 92 implies check_and_wait would return BelowThreshold"
        );
    }

    #[test]
    fn test_parse_oauth_usage_weekly_all_100_still_gates() {
        // Inverse of the live fixture: narrowing must not disable the real weekly gate.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 20.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "limits": [
                {
                    "kind": "session",
                    "percent": 20,
                    "severity": "normal",
                    "resets_at": "2026-09-07T06:00:00Z",
                    "is_active": true
                },
                {
                    "kind": "weekly_all",
                    "percent": 100,
                    "severity": "critical",
                    "resets_at": "2026-09-12T19:00:00Z",
                    "is_active": true
                }
            ]
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!((info.percentage - 100.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-12T19:00:00Z"),
            "weekly_all 100 ≥ 92 → reset_at is weekly, not session"
        );
    }

    #[test]
    fn test_parse_oauth_usage_latest_among_gate_relevant() {
        // Several account-binding windows ≥ 92 → latest timestamp, not soonest.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 95.0,
                "resets_at": "2026-09-07T08:00:00Z"
            },
            "seven_day": {
                "utilization": 93.0,
                "resets_at": "2026-09-13T19:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!((info.percentage - 95.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-13T19:00:00Z"),
            "latest among ≥92 must win over soonest session reset"
        );
    }

    #[test]
    fn test_parse_oauth_usage_band_95_50_uses_weekly_reset() {
        // Known-bad for exhausted=≥100: percentage would wait but reset_at would
        // stay session. Gate-relevant (≥92) must pick weekly.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 50.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 95.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "limits": [
                {
                    "kind": "session",
                    "percent": 50,
                    "severity": "normal",
                    "resets_at": "2026-09-07T06:00:00Z",
                    "is_active": true
                },
                {
                    "kind": "weekly_all",
                    "percent": 95.0,
                    "severity": "normal",
                    "resets_at": "2026-09-12T19:00:00Z",
                    "is_active": false
                }
            ]
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!((info.percentage - 95.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-12T19:00:00Z"),
            "95 ≥ 92 weekly must win; exhausted=≥100 would wrongly keep session"
        );
    }

    #[test]
    fn test_parse_oauth_usage_live_threshold_80_weekly_85() {
        // LOOP_USAGE_THRESHOLD=80: weekly 85 is gate-relevant for the wait, so
        // reset_at must be weekly — not session (compile-time 92 would miss it).
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 50.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 85.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "limits": [
                {
                    "kind": "session",
                    "percent": 50,
                    "severity": "normal",
                    "resets_at": "2026-09-07T06:00:00Z",
                    "is_active": true
                },
                {
                    "kind": "weekly_all",
                    "percent": 85,
                    "severity": "normal",
                    "resets_at": "2026-09-12T19:00:00Z",
                    "is_active": false
                }
            ]
        });
        let info =
            parse_oauth_usage_json_with_threshold(&json, 80.0).expect("must parse at threshold 80");
        assert!((info.percentage - 85.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-12T19:00:00Z"),
            "85 ≥ live threshold 80 → reset_at is weekly"
        );
        // Default-92 path still prefers session (85 < 92) — the bug this fixes.
        let info_default = parse_oauth_usage_json(&json).expect("default parse");
        assert_eq!(
            info_default.reset_at.as_deref(),
            Some("2026-09-07T06:00:00Z"),
            "default 92 must still prefer session when weekly is 85"
        );
    }

    #[test]
    fn test_parse_oauth_usage_scoped_critical_does_not_exhaust() {
        // severity=critical + is_active on weekly_scoped must not enter fold.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 10.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 30.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "limits": [
                {
                    "kind": "weekly_scoped",
                    "percent": 99,
                    "severity": "critical",
                    "is_active": true,
                    "resets_at": "2026-09-12T19:00:00Z",
                    "scope": {
                        "model": { "display_name": "Fable" }
                    }
                }
            ]
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!((info.percentage - 30.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-07T06:00:00Z"),
            "scoped critical must not set reset_at or raise percentage"
        );
    }

    #[test]
    fn test_parse_oauth_usage_named_opus_sonnet_dropped() {
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 40.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 40.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "seven_day_opus": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "seven_day_sonnet": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("must parse");
        assert!(
            (info.percentage - 40.0).abs() < f64::EPSILON,
            "named opus/sonnet at 100 must not raise percentage; got {}",
            info.percentage
        );
        assert_eq!(info.reset_at.as_deref(), Some("2026-09-07T06:00:00Z"));
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
        assert!(
            OAUTH_USAGE_USER_AGENT_FALLBACK.starts_with("claude-code/"),
            "OAuth usage UA must look like Claude Code or the endpoint 429s probes"
        );
    }

    #[test]
    fn test_parse_claude_version_live_cli_shape() {
        assert_eq!(
            parse_claude_version_output("2.1.257 (Claude Code)\n"),
            Some("2.1.257".to_string())
        );
    }

    #[test]
    fn test_parse_claude_version_bare_semver() {
        assert_eq!(
            parse_claude_version_output("2.1.257\n"),
            Some("2.1.257".to_string())
        );
    }

    #[test]
    fn test_parse_claude_version_prefixed_line() {
        assert_eq!(
            parse_claude_version_output("claude 2.0.14 extra"),
            Some("2.0.14".to_string())
        );
    }

    #[test]
    fn test_parse_claude_version_garbage_is_none() {
        assert_eq!(parse_claude_version_output(""), None);
        assert_eq!(parse_claude_version_output("not-a-version\n"), None);
        assert_eq!(
            parse_claude_version_output("2\n"),
            None,
            "bare major is not dotted"
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

    // --- ingest_oauth_value (PR-2 / FEAT-003) ---

    fn builtin_models() -> ResolvedModelsConfig {
        use crate::loop_engine::project_config::{ModelsConfig, RoutingConfig};
        crate::loop_engine::model::resolve_models_config(
            &ModelsConfig::builtin_default(),
            &RoutingConfig::default(),
        )
    }

    fn models_with_frontier_pinned_to_standard() -> ResolvedModelsConfig {
        use crate::loop_engine::model::{
            FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL, resolve_models_config,
        };
        use crate::loop_engine::project_config::{ModelsConfig, ProviderConfig, RoutingConfig};
        use std::collections::HashMap;

        let mut providers = HashMap::new();
        providers.insert(
            Provider::Claude.as_str().to_string(),
            ProviderConfig {
                enabled: true,
                tiers: [
                    (CapabilityTier::Cheapest, Some(HAIKU_MODEL)),
                    (CapabilityTier::CostEfficient, Some(SONNET_MODEL)),
                    (CapabilityTier::Standard, Some(OPUS_MODEL)),
                    // PR-1 pin: frontier shares the standard model string.
                    (CapabilityTier::Frontier, Some(OPUS_MODEL)),
                ]
                .into_iter()
                .map(|(t, m)| (t.as_str().to_string(), m.map(str::to_string)))
                .collect(),
                effort: HashMap::new(),
                fallback: None,
                cli_binary: None,
            },
        );
        // Keep FABLE_MODEL referenced so the pin contrast is explicit in review.
        let _ = FABLE_MODEL;
        let models = ModelsConfig {
            primary_provider: Provider::Claude.as_str().to_string(),
            anchor: CapabilityTier::Standard.as_str().to_string(),
            providers,
        };
        resolve_models_config(&models, &RoutingConfig::default())
    }

    fn live_shaped_oauth_json() -> serde_json::Value {
        serde_json::json!({
            "five_hour": {
                "utilization": 24.0,
                "resets_at": "2026-09-07T06:00:00Z"
            },
            "seven_day": {
                "utilization": 55.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "seven_day_opus": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "seven_day_sonnet": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "nimbus_quill": {
                "utilization": 100.0,
                "resets_at": "2026-09-12T19:00:00Z"
            },
            "spend": {
                "dollars": 12.5
            },
            "null_window": null,
            "limits": [
                {
                    "kind": "session",
                    "percent": 24,
                    "severity": "normal",
                    "resets_at": "2026-09-07T06:00:00Z",
                    "is_active": true
                },
                {
                    "kind": "weekly_all",
                    "percent": 55.0,
                    "severity": "normal",
                    "resets_at": "2026-09-12T19:00:00Z",
                    "is_active": false
                },
                {
                    "kind": "weekly_scoped",
                    "percent": 95,
                    "severity": "critical",
                    "is_active": true,
                    "resets_at": "2026-09-12T19:00:00Z",
                    "scope": {
                        "model": { "display_name": "Fable" }
                    }
                }
            ]
        })
    }

    fn bucket_by_id<'a>(buckets: &'a [QuotaBucket], id: &str) -> &'a QuotaBucket {
        buckets
            .iter()
            .find(|b| b.id == id)
            .unwrap_or_else(|| panic!("missing bucket id {id}"))
    }

    fn percent_of(bucket: &QuotaBucket) -> f64 {
        bucket
            .measurements
            .iter()
            .find(|m| m.unit == MeasurementUnit::Percent)
            .map(|m| m.remaining)
            .expect("percent measurement")
    }

    #[test]
    fn ingest_live_fixture_emits_all_siblings_and_limits() {
        let models = builtin_models();
        let buckets = ingest_oauth_value(&live_shaped_oauth_json(), &models);

        let ids: Vec<&str> = buckets.iter().map(|b| b.id.as_str()).collect();
        for expected in [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "nimbus_quill",
            "spend",
            "limits[0].session",
            "limits[1].weekly_all",
            "limits[2].weekly_scoped",
        ] {
            assert!(ids.contains(&expected), "expected {expected} in {ids:?}");
        }
        assert!(
            !ids.iter().any(|id| id.contains("null_window")),
            "null siblings must skip"
        );

        assert!((percent_of(bucket_by_id(&buckets, "five_hour")) - 76.0).abs() < f64::EPSILON);
        assert!((percent_of(bucket_by_id(&buckets, "seven_day")) - 45.0).abs() < f64::EPSILON);
        assert!(
            (percent_of(bucket_by_id(&buckets, "limits[2].weekly_scoped")) - 5.0).abs()
                < f64::EPSILON
        );

        let spend = bucket_by_id(&buckets, "spend");
        assert_eq!(spend.kind, "spend");
        assert!(spend.measurements.iter().any(
            |m| m.unit == MeasurementUnit::Dollars && (m.remaining - 12.5).abs() < f64::EPSILON
        ));

        // Account-binding named windows have no rungs.
        assert!(bucket_by_id(&buckets, "five_hour").rungs.is_none());
        assert!(bucket_by_id(&buckets, "seven_day").rungs.is_none());

        // Fable HUD → frontier only under default ladder (no pin).
        let fable = bucket_by_id(&buckets, "limits[2].weekly_scoped");
        assert_eq!(
            fable.rungs.as_deref(),
            Some(&[(Provider::Claude, CapabilityTier::Frontier)][..])
        );
        assert_eq!(fable.severity.as_deref(), Some("critical"));
        assert_eq!(fable.is_active, Some(true));

        // Unlabeled seven_day_opus / sonnet via configured model substring.
        let opus = bucket_by_id(&buckets, "seven_day_opus");
        assert_eq!(opus.kind, "weekly_scoped");
        assert_eq!(
            opus.rungs.as_deref(),
            Some(&[(Provider::Claude, CapabilityTier::Standard)][..])
        );
        let sonnet = bucket_by_id(&buckets, "seven_day_sonnet");
        assert_eq!(
            sonnet.rungs.as_deref(),
            Some(&[(Provider::Claude, CapabilityTier::CostEfficient)][..])
        );

        // Unknown family → no rungs (evaluate ignores under default policy).
        assert!(bucket_by_id(&buckets, "nimbus_quill").rungs.is_none());
    }

    #[test]
    fn ingest_extra_mark_after_frontier_pin_marks_standard_and_frontier() {
        let models = models_with_frontier_pinned_to_standard();
        let json = serde_json::json!({
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 95,
                "scope": { "model": { "display_name": "Opus" } }
            }]
        });
        let buckets = ingest_oauth_value(&json, &models);
        let b = bucket_by_id(&buckets, "limits[0].weekly_scoped");
        let rungs = b.rungs.as_ref().expect("Opus HUD must map rungs");
        assert!(
            rungs.contains(&(Provider::Claude, CapabilityTier::Standard)),
            "HUD Opus → standard; got {rungs:?}"
        );
        assert!(
            rungs.contains(&(Provider::Claude, CapabilityTier::Frontier)),
            "extra-mark must also mark frontier when it shares the standard model string; got {rungs:?}"
        );
    }

    #[test]
    fn ingest_hud_only_without_pin_leaves_frontier_unmarked_on_opus() {
        let models = builtin_models();
        let json = serde_json::json!({
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 95,
                "scope": { "model": { "display_name": "Current week (Opus)" } }
            }]
        });
        let buckets = ingest_oauth_value(&json, &models);
        let b = bucket_by_id(&buckets, "limits[0].weekly_scoped");
        assert_eq!(
            b.rungs.as_deref(),
            Some(&[(Provider::Claude, CapabilityTier::Standard)][..]),
            "HUD-only Opus must NOT mark frontier without the pin"
        );
    }

    #[test]
    fn ingest_skips_malformed_and_accepts_u64_or_f64_percent() {
        let models = builtin_models();
        let json = serde_json::json!({
            "five_hour": { "utilization": 10.0 },
            "broken": { "nope": true },
            "limits": [
                { "kind": "session", "percent": 10 },
                { "kind": "weekly_all", "percent": 20.5 },
                { "kind": "weekly_scoped" },
                null
            ]
        });
        let buckets = ingest_oauth_value(&json, &models);
        let ids: Vec<&str> = buckets.iter().map(|b| b.id.as_str()).collect();
        assert!(ids.contains(&"five_hour"));
        assert!(ids.contains(&"limits[0].session"));
        assert!(ids.contains(&"limits[1].weekly_all"));
        assert!(!ids.contains(&"broken"));
        assert!(!ids.iter().any(|id| id.contains("weekly_scoped")));
        assert!(
            (percent_of(bucket_by_id(&buckets, "limits[0].session")) - 90.0).abs() < f64::EPSILON
        );
        assert!(
            (percent_of(bucket_by_id(&buckets, "limits[1].weekly_all")) - 79.5).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn parse_oauth_usage_json_signature_stays_threshold_only() {
        // Compile-time guard: PR-1 fold must not grow a models param.
        let _f: fn(&serde_json::Value, f64) -> Option<UsageInfo> =
            parse_oauth_usage_json_with_threshold;
    }
}
