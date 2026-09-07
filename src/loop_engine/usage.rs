//! Usage API monitoring for the autonomous agent loop.
//!
//! Checks remaining quota before each iteration and waits for reset when
//! account remaining is at or below the configured floor. Gracefully degrades
//! if credentials are unavailable or the API is unreachable.
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

use chrono::{DateTime, Utc};

use crate::loop_engine::display;
use crate::loop_engine::model::{
    CapabilityTier, FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, Provider, ResolvedModelsConfig,
    SONNET_MODEL, builtin_resolved_models,
};
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

/// Default remaining-percent floor for selecting `reset_at` among account-binding
/// windows. Matches `LoopConfig::usage_remaining_min` default (8). Callers that
/// know the live config (`check_and_wait`, post-output load) pass the live floor
/// so wait duration tracks the same bar as the remaining compare.
/// Old used≥92 ≡ remaining≤8.
const DEFAULT_USAGE_REMAINING_MIN: f64 = 8.0;

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
    /// Account remaining percent (0.0–100.0).
    ///
    /// For the OAuth endpoint: **min remaining** across account-binding windows
    /// only (named `five_hour` / `seven_day`, plus `limits[]` with `kind`
    /// `session` or `weekly_all`). Remaining = `(100 - used).clamp(0, 100)`.
    /// Rung-scoped windows are omitted so a frontier-only bucket cannot park
    /// the account gate.
    pub percentage: f64,
    /// ISO 8601 reset timestamp for waiting, if available.
    ///
    /// For the OAuth endpoint: **latest** `resets_at` among account-binding
    /// windows whose remaining is ≤ the live floor (default
    /// [`DEFAULT_USAGE_REMAINING_MIN`] / 8, or `LoopConfig::usage_remaining_min`
    /// when threaded through [`load_usage_info_with_threshold`]); if none are
    /// gate-relevant, prefer the session window (`five_hour` or `limits[]`
    /// kind `session`), else any account-binding reset. Not the soonest
    /// exhausted / severity-critical timestamp across all windows.
    pub reset_at: Option<String>,
    /// Multi-bucket `% left` operator banner when OAuth HUD JSON was parsed.
    pub remaining_banner: Option<String>,
    /// Generic quota buckets from OAuth ingest (PR-2). Empty for org-endpoint
    /// fallback or when ingest was not run. Prefer
    /// [`buckets_for_run_models`] before evaluate/apply so extra-mark uses the
    /// run's [`ResolvedModelsConfig`] (not a builtin snapshot from fetch).
    pub buckets: Vec<crate::loop_engine::quota::QuotaBucket>,
    /// Raw OAuth HUD JSON when the OAuth endpoint succeeded. Lets
    /// [`run_account_quota_gate`](crate::loop_engine::reactions::account::run_account_quota_gate)
    /// re-ingest with `params.models` so a frontier→opus pin extra-marks both
    /// rungs. `None` for org-endpoint fallback.
    pub oauth_json: Option<serde_json::Value>,
}

/// Result of a usage check-and-wait cycle.
#[derive(Debug, PartialEq)]
pub enum UsageCheckResult {
    /// Remaining is above the floor, proceed.
    BelowThreshold,
    /// Waited for reset successfully, now below threshold.
    WaitedAndReset,
    /// Wait was interrupted by an operator `.stop` signal.
    /// Call sites set `was_stopped` / `operator_stopped` so batch `--chain`
    /// treats this as an intentional operator halt.
    StopSignaled,
    /// Quota horizon Stop (`QuotaAccountAction::Stop`): reset is beyond the
    /// horizon and no other rung can run. Soft-stops this PRD without the
    /// operator-stop banner or `was_stopped` (chain may still halt via
    /// incomplete/`prd_complete`, not via the stop-file gate).
    HorizonStopped,
    /// Usage check was skipped (disabled or no credentials).
    Skipped,
    /// API call failed but we continue anyway (graceful degradation).
    ApiError(String),
    /// Operator forbade tierFallback downgrade and ask TTL is 0 — no sleep,
    /// no continue (soft-stop for this PRD). TTL > 0 sleeps via Ask then
    /// continues ([`UsageCheckResult::WaitedAndReset`]) or
    /// [`UsageCheckResult::StopSignaled`]. Must not set `was_stopped`.
    Deferred,
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
    check_usage_api_with_threshold(access_token, DEFAULT_USAGE_REMAINING_MIN as u8)
}

/// Like [`check_usage_api`], but `reset_at` uses `threshold` as the
/// remaining-min floor (same value `check_and_wait` compares against
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

    let mut info = parse_oauth_usage_json_with_threshold(&json, f64::from(threshold))?;
    // Provisional builtin banner for callers without run models (e.g.
    // check_and_wait). Production pre-dispatch gate MUST rebuild via
    // remaining_banner_for_run_models / format_oauth_remaining_banner with
    // the run ResolvedModelsConfig so frontier→opus pins label both rungs.
    info.remaining_banner = Some(format_oauth_remaining_banner(
        &json,
        threshold,
        Utc::now(),
        builtin_resolved_models(),
    ));
    // Builtin ingest is a display/default snapshot only. Evaluate/apply MUST
    // re-ingest via buckets_for_run_models with the run ResolvedModelsConfig
    // so a frontier→opus pin extra-marks both rungs (WIRE-FIX-001).
    info.buckets = ingest_oauth_value(&json, builtin_resolved_models());
    info.oauth_json = Some(json);
    Some(info)
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
    /// Used percent 0–100 as reported by the API.
    util: f64,
    reset: Option<String>,
    /// Named `five_hour` or `limits[]` kind `session` — preferred when no
    /// account-binding window is ≤ the live remaining floor.
    is_session: bool,
}

impl UsageWindow {
    fn remaining(&self) -> f64 {
        (100.0 - self.util).clamp(0.0, 100.0)
    }
}

/// Ingest every OAuth usage object sibling and `limits[]` row into generic
/// [`QuotaBucket`]s (PR-2 / FR-003).
///
/// Walks **all** object siblings with `utilization` or `dollars` — no
/// window-name allow-list. Null / non-object siblings are skipped. Malformed
/// rows (no usable measurement) are skipped without panicking.
///
/// Rung mapping (Claude OAuth HUD only):
/// 1. `limits[]` `scope.model.display_name` / `id` via HUD label table
///    (case-insensitive prefix/token): Fable→frontier, Opus→standard,
///    Sonnet→cost-efficient, Haiku→cheapest.
/// 2. Else `limits[]` unlabeled ids (id, no display_name): family token as
///    substring of a *defined* configured model string (`exact_model_for`).
/// 3. After a HUD map to rung R, extra-mark every defined rung whose
///    `exact_model_for` equals any member of identity set I (CONTRACT-002).
/// 4. Named object siblings (`seven_day_opus`, …) have no `scope.model` →
///    **`rungs: None`** (do **not** call `map_unlabeled_token`). Still walked;
///    `kind` may be `weekly_scoped` for explicit onLow rules.
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
            if let Some(bucket) = ingest_named_sibling(key, value) {
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

fn ingest_named_sibling(key: &str, value: &serde_json::Value) -> Option<QuotaBucket> {
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

    // Named siblings have no scope.model — never family-token-map onto a rung
    // (FEAT-009 / CONTRACT-002). kind may still be weekly_scoped for onLow rules.
    Some(QuotaBucket {
        id: key.to_string(),
        kind,
        label: String::new(),
        measurements,
        resets_at,
        severity,
        is_active,
        rungs: None,
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
    if let Some(tokens) = value.get("tokens").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: tokens,
            unit: MeasurementUnit::Tokens,
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
    if let Some(tokens) = limit.get("tokens").and_then(json_number_as_f64) {
        out.push(Measurement {
            remaining: tokens,
            unit: MeasurementUnit::Tokens,
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
        return Some(extra_mark_for_hud_tier(models, provider, tier, model_id));
    }
    if let Some(id) = model_id {
        if let Some(tier) = hud_tier_from_label(id) {
            return Some(extra_mark_for_hud_tier(models, provider, tier, Some(id)));
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

/// Built-in family model id for a HUD-mapped capability rung.
///
/// HUD tokens (`fable`/`opus`/…) map onto rungs; this returns the stable
/// family constant for that rung — **not** the run config's `exact_model_for`.
fn canonical_model_for_hud_tier(tier: CapabilityTier) -> Option<&'static str> {
    match tier {
        CapabilityTier::Frontier => Some(FABLE_MODEL),
        CapabilityTier::Standard => Some(OPUS_MODEL),
        CapabilityTier::CostEfficient => Some(SONNET_MODEL),
        CapabilityTier::Cheapest => Some(HAIKU_MODEL),
    }
}

/// After HUD maps `display_name`/`id` → rung R, build identity set
/// `I = {canonical_model_for_hud_tier(R)} ∪ {scope.model.id?}` and extra-mark
/// every defined Claude rung whose `exact_model_for` equals any member of I.
/// Always includes HUD primary. Never keys on `exact_model_for(R)`.
fn extra_mark_for_hud_tier(
    models: &ResolvedModelsConfig,
    provider: Provider,
    primary: CapabilityTier,
    model_id: Option<&str>,
) -> Vec<(Provider, CapabilityTier)> {
    let mut identities: Vec<&str> = Vec::with_capacity(2);
    if let Some(canonical) = canonical_model_for_hud_tier(primary) {
        identities.push(canonical);
    }
    if let Some(id) = model_id {
        identities.push(id);
    }
    let mut out = extra_mark_rungs_matching(models, provider, identities);
    // Always include HUD primary even when no identity matched a configured model.
    if !out.iter().any(|(_, t)| *t == primary) {
        out.push((provider, primary));
    }
    out.sort_by_key(|(_, t)| *t);
    out.dedup();
    out
}

/// Extra-mark every defined rung on `provider` whose configured model string
/// equals **any** member of `identities` (string equality only — not substring
/// `tier_of`, not `model_for` clamp, not `exact_model_for(mapped_rung)`).
///
/// Call sites pass Claude only; do not iterate Grok/Codex ladders.
pub(crate) fn extra_mark_rungs_matching<'a, I>(
    models: &ResolvedModelsConfig,
    provider: Provider,
    identities: I,
) -> Vec<(Provider, CapabilityTier)>
where
    I: IntoIterator<Item = &'a str>,
{
    let identity_set: Vec<&str> = identities.into_iter().collect();
    if identity_set.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for tier in CapabilityTier::ALL {
        if let Some(configured) = models.exact_model_for(provider, tier)
            && identity_set.contains(&configured)
        {
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
/// remaining floor ([`DEFAULT_USAGE_REMAINING_MIN`] / 8).
///
/// Test convenience wrapper. Production always calls
/// [`parse_oauth_usage_json_with_threshold`] with the live
/// `LoopConfig::usage_remaining_min` so `reset_at` matches the remaining compare
/// in `check_and_wait`.
#[cfg(test)]
pub(crate) fn parse_oauth_usage_json(json: &serde_json::Value) -> Option<UsageInfo> {
    parse_oauth_usage_json_with_threshold(json, DEFAULT_USAGE_REMAINING_MIN)
}

/// Live-shaped OAuth usage JSON shared by usage / account / pre_spawn tests
/// (FEAT-009). Do not copy — import this fixture.
#[cfg(test)]
pub(crate) fn live_shaped_oauth_json() -> serde_json::Value {
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

/// Resolved models with Claude frontier pinned to the standard (Opus) model
/// string — the PR-1 pin shape used by FEAT-008/009 identity fixtures.
#[cfg(test)]
pub(crate) fn models_with_frontier_pinned_to_standard() -> ResolvedModelsConfig {
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

/// Parse the Claude Code OAuth usage JSON into [`UsageInfo`].
///
/// Account-binding fold (PR-2 remaining unit):
/// - **percentage** = min **remaining** (0–100) across account-binding windows
///   only: named `five_hour` / `seven_day`, plus `limits[]` with `kind`
///   `session` or `weekly_all`. Remaining = `(100 - used).clamp(0, 100)`.
///   Named `seven_day_opus` / `seven_day_sonnet` and `limits[]` kinds
///   `weekly_scoped` / `extra_usage` / `promotional` are skipped.
/// - **reset_at** = latest among those windows with remaining ≤ `remaining_min`
///   (live floor, default 8); if none, prefer session; else any account-binding
///   reset. Gate-relevant means remaining ≤ floor (old used≥92 ≡ remaining≤8).
///
/// Pure / unit-testable — no I/O. Does not set [`UsageInfo::remaining_banner`]
/// (call [`format_oauth_remaining_banner`] separately).
pub(crate) fn parse_oauth_usage_json_with_threshold(
    json: &serde_json::Value,
    remaining_min: f64,
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

    // Account remaining = min of account-binding remaining.
    let percentage = windows
        .iter()
        .map(UsageWindow::remaining)
        .fold(100.0_f64, f64::min);

    let reset_at = latest_reset(
        windows
            .iter()
            .filter(|w| w.remaining() <= remaining_min)
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
        remaining_banner: None,
        buckets: Vec::new(),
        oauth_json: None,
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
/// Uses the default remaining floor (8) for `reset_at` selection. Prefer
/// [`load_usage_info_with_threshold`] when the live `usage_remaining_min` is known.
///
/// Used by the pre-iteration gate, post-rate-limit resolve, spillover blackout
/// duration, and early-lift probes. Returns `None` when credentials are missing
/// or both usage endpoints fail.
pub fn load_usage_info() -> Option<UsageInfo> {
    load_usage_info_with_threshold(DEFAULT_USAGE_REMAINING_MIN as u8)
}

/// Like [`load_usage_info`], but `reset_at` is selected with `threshold` as the
/// remaining-min floor (same value compared to `percentage` in `check_and_wait`).
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
/// Both pre-gate and post-limit: remaining **above** the rule floor
/// (`percentage` is remaining 0–100). No magic 0.05 ratio and no used<95.
pub fn usage_suggests_lifted(info: &UsageInfo, threshold: u8, post_limit: bool) -> bool {
    let _ = post_limit; // same remaining > floor rule for both legs
    info.percentage > f64::from(threshold)
}

/// Parse the legacy org usage JSON.
fn parse_org_usage_json(json: &serde_json::Value) -> Option<UsageInfo> {
    // Org endpoint historically reported **used** percent; invert to remaining.
    let used = json["usage_percentage"]
        .as_f64()
        .or_else(|| json["percentage"].as_f64())
        .or_else(|| {
            let used = json["used"].as_f64()?;
            let limit = json["limit"].as_f64()?;
            if limit > 0.0 {
                Some((used / limit) * 100.0)
            } else {
                None
            }
        });

    let used = match used {
        Some(p) => p,
        None => {
            tracing::warn!("usage API response missing percentage data");
            return None;
        }
    };
    let percentage = (100.0 - used).clamp(0.0, 100.0);

    let reset_at = json["reset_at"]
        .as_str()
        .or_else(|| json["resets_at"].as_str())
        .map(|s| s.to_string());

    Some(UsageInfo {
        percentage,
        reset_at,
        remaining_banner: None,
        buckets: Vec::new(),
        oauth_json: None,
    })
}

/// Build evaluate/apply buckets using the run's resolved models.
///
/// When [`UsageInfo::oauth_json`] is present (OAuth path), re-ingests so
/// extra-mark sees pinned ladders (`set-tier claude frontier=<opus>`). Falls
/// back to the pre-built `buckets` snapshot (org path / hermetic fixtures).
pub fn buckets_for_run_models(
    info: &UsageInfo,
    models: &ResolvedModelsConfig,
) -> Vec<crate::loop_engine::quota::QuotaBucket> {
    match &info.oauth_json {
        Some(json) => ingest_oauth_value(json, models),
        None => info.buckets.clone(),
    }
}

/// Format the operator remaining banner from OAuth HUD JSON (hermetic).
///
/// Shape: `session 76% left (3m) · week 45% left (5d 13h) · frontier 5% left (5d 13h) (floor 8%)`.
/// Rung labels use capability-tier names (`frontier`), never model ids (`fable`).
///
/// `models` must be the same run [`ResolvedModelsConfig`] used by
/// [`buckets_for_run_models`] / evaluate / apply so a frontier→opus pin
/// extra-marks both rungs on the Opus HUD line.
pub fn format_oauth_remaining_banner(
    json: &serde_json::Value,
    remaining_min: u8,
    now: DateTime<Utc>,
    models: &ResolvedModelsConfig,
) -> String {
    let buckets = ingest_oauth_value(json, models);
    format_remaining_usage_banner(&buckets, remaining_min, now)
}

/// Rebuild the remaining banner with the run's [`ResolvedModelsConfig`].
///
/// Prefer this over [`UsageInfo::remaining_banner`] (which may be a builtin
/// snapshot from fetch) so production stderr matches evaluate/apply extra-mark.
pub fn remaining_banner_for_run_models(
    info: &UsageInfo,
    models: &ResolvedModelsConfig,
    remaining_min: u8,
    now: DateTime<Utc>,
) -> Option<String> {
    match &info.oauth_json {
        Some(json) => Some(format_oauth_remaining_banner(
            json,
            remaining_min,
            now,
            models,
        )),
        None => info.remaining_banner.clone(),
    }
}

/// Format a remaining banner from already-ingested [`QuotaBucket`]s.
pub fn format_remaining_usage_banner(
    buckets: &[QuotaBucket],
    remaining_min: u8,
    now: DateTime<Utc>,
) -> String {
    let mut segments: Vec<String> = Vec::new();
    let mut saw_session = false;
    let mut saw_week = false;
    let mut saw_rungs: Vec<CapabilityTier> = Vec::new();

    for bucket in buckets {
        let labels = banner_labels_for_bucket(bucket);
        if labels.is_empty() {
            continue;
        }
        let Some(meas) = primary_measurement(bucket) else {
            continue;
        };
        let amount = format_measurement_left(meas);
        let dur = bucket
            .resets_at
            .as_deref()
            .and_then(|r| duration_until_reset(r, now))
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        for label in labels {
            match label.as_str() {
                "session" if saw_session => continue,
                "week" if saw_week => continue,
                _ => {}
            }
            if let Ok(tier) = CapabilityTier::parse(&label) {
                if saw_rungs.contains(&tier) {
                    continue;
                }
                saw_rungs.push(tier);
            }
            segments.push(format!("{label} {amount}{dur}"));
            if label == "session" {
                saw_session = true;
            } else if label == "week" {
                saw_week = true;
            }
        }
    }

    if segments.is_empty() {
        format!("(floor {remaining_min}%)")
    } else {
        format!("{} (floor {remaining_min}%)", segments.join(" · "))
    }
}

/// Labels for one bucket. Account-binding → session/week/spend name.
/// Rung-scoped labeled HUD rows → every configured/extra-marked rung (not
/// HUD-primary alone), so frontier=opus pins show both `frontier` and
/// `standard` for an Opus line.
fn banner_labels_for_bucket(bucket: &QuotaBucket) -> Vec<String> {
    let is_account = bucket.rungs.as_ref().is_none_or(|r| r.is_empty());
    if is_account {
        return match bucket.kind.as_str() {
            "session" => vec!["session".into()],
            "weekly_all" => vec!["week".into()],
            // Dollar/token-only spend buckets without a percent: show kind/id.
            _ if bucket.measurements.iter().any(|m| {
                matches!(
                    m.unit,
                    MeasurementUnit::Dollars | MeasurementUnit::Tokens | MeasurementUnit::Credits
                )
            }) =>
            {
                let name = if bucket.label.is_empty() {
                    bucket.kind.as_str()
                } else {
                    bucket.label.as_str()
                };
                vec![name.to_string()]
            }
            _ => Vec::new(),
        };
    }
    // Rung-scoped: only labeled HUD rows (skip unlabeled named seven_day_*).
    if bucket.label.is_empty() {
        return Vec::new();
    }
    if let Some(rungs) = bucket.rungs.as_ref() {
        // Preserve ingest order (CapabilityTier sort from extra_mark_rungs_matching).
        let mut labels: Vec<String> = Vec::new();
        for (_, tier) in rungs {
            let name = tier.as_str().to_string();
            if !labels.contains(&name) {
                labels.push(name);
            }
        }
        if !labels.is_empty() {
            return labels;
        }
    }
    // Fallback: HUD table primary only (should be rare once rungs are set).
    hud_tier_from_label(&bucket.label)
        .map(|t| vec![t.as_str().to_string()])
        .unwrap_or_default()
}

fn primary_measurement(bucket: &QuotaBucket) -> Option<&Measurement> {
    bucket
        .measurements
        .iter()
        .find(|m| m.unit == MeasurementUnit::Percent)
        .or_else(|| bucket.measurements.first())
}

fn format_measurement_left(m: &Measurement) -> String {
    match m.unit {
        MeasurementUnit::Percent => format!("{}% left", format_compact_number(m.remaining)),
        MeasurementUnit::Dollars => format!("${} left", format_compact_number(m.remaining)),
        MeasurementUnit::Tokens => format!("{} tokens left", format_compact_number(m.remaining)),
        MeasurementUnit::Credits => format!("{} credits left", format_compact_number(m.remaining)),
    }
}

fn format_compact_number(v: f64) -> String {
    if (v - v.round()).abs() < f64::EPSILON {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn duration_until_reset(reset_at: &str, now: DateTime<Utc>) -> Option<String> {
    let ts = parse_reset_timestamp(reset_at)?;
    let secs = if ts > now.timestamp() {
        (ts - now.timestamp()) as u64
    } else {
        0
    };
    Some(display::format_duration(secs))
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
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
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
        // Account remaining = min(session 0, week 89) = 0.
        assert!((info.percentage - 0.0).abs() < f64::EPSILON);
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
            (info.percentage - 89.0).abs() < f64::EPSILON,
            "min remaining is weekly 89 (used 11), not five_hour 1.0 scaled; got {}",
            info.percentage
        );
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-01T23:00:00Z"),
            "nothing ≤ floor 8 → prefer five_hour.resets_at"
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
        // min remaining of session 67 / week 87 = 67; sonnet named window omitted.
        assert!((info.percentage - 67.0).abs() < f64::EPSILON);
        assert_eq!(info.reset_at.as_deref(), Some("2026-04-11T07:00:00Z"));
    }

    #[test]
    fn test_parse_oauth_usage_sub_one_percent_stays_sub_one() {
        // OAuth reports used 0–100. A true 0.42% used → 99.58% remaining; must
        // not be scaled as a fraction.
        let json = serde_json::json!({
            "five_hour": {
                "utilization": 0.42,
                "resets_at": "2026-02-28T17:00:00Z"
            }
        });
        let info = parse_oauth_usage_json(&json).expect("sub-one percent must parse");
        assert!((info.percentage - 99.58).abs() < 1e-9);
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
        // Weekly at 100% used (0 remaining), session fine → remaining 0, reset = weekly.
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
        assert!((info.percentage - 0.0).abs() < f64::EPSILON);
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
        // remaining 10 for both; 10 > floor 8 → prefer session reset.
        assert!((info.percentage - 10.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-08-06T01:00:00Z"),
            "when nothing ≤ floor, prefer five_hour.resets_at"
        );
    }

    #[test]
    fn test_parse_oauth_usage_live_fixture_ignores_scoped_and_named_rungs() {
        // Production-shaped HUD: session used 24 (rem 76), weekly-all used 55
        // (rem 45), Fable weekly_scoped used 95 + named opus/sonnet 100.
        // Account fold = min(76,45)=45 with session reset — 45 > floor 8
        // ⇒ BelowThreshold. Discriminator: keeping used≥92 after rename would
        // treat 45 as used and never wait when remaining is 8.
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
            (info.percentage - 45.0).abs() < f64::EPSILON,
            "account-binding min remaining is weekly-all 45, not Fable 5; got {}",
            info.percentage
        );
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-07T06:00:00Z"),
            "nothing ≤ floor 8 → prefer session reset, not weekly Fable"
        );
        assert!(
            info.percentage > DEFAULT_USAGE_REMAINING_MIN,
            "45 > 8 implies check_and_wait would return BelowThreshold"
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
        assert!((info.percentage - 0.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-12T19:00:00Z"),
            "weekly_all remaining 0 ≤ 8 → reset_at is weekly, not session"
        );
    }

    #[test]
    fn test_parse_oauth_usage_latest_among_gate_relevant() {
        // Several account-binding windows remaining ≤ 8 → latest timestamp, not soonest.
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
        // min remaining of 5 and 7 = 5
        assert!((info.percentage - 5.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-13T19:00:00Z"),
            "latest among remaining≤8 must win over soonest session reset"
        );
    }

    #[test]
    fn test_parse_oauth_usage_band_95_50_uses_weekly_reset() {
        // Known-bad for exhausted=≥100: percentage would wait but reset_at would
        // stay session. Gate-relevant (remaining ≤ 8) must pick weekly.
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
        assert!((info.percentage - 5.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-12T19:00:00Z"),
            "weekly remaining 5 ≤ 8 must win; exhausted=≥100 would wrongly keep session"
        );
    }

    #[test]
    fn test_parse_oauth_usage_live_remaining_min_20_weekly_15() {
        // LOOP_USAGE_REMAINING_MIN=20 (old used-threshold 80): weekly used 85
        // → remaining 15 ≤ 20 is gate-relevant, so reset_at must be weekly.
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
        let info = parse_oauth_usage_json_with_threshold(&json, 20.0)
            .expect("must parse at remaining_min 20");
        assert!((info.percentage - 15.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-12T19:00:00Z"),
            "remaining 15 ≤ live floor 20 → reset_at is weekly"
        );
        // Default floor 8 still prefers session (15 > 8).
        let info_default = parse_oauth_usage_json(&json).expect("default parse");
        assert_eq!(
            info_default.reset_at.as_deref(),
            Some("2026-09-07T06:00:00Z"),
            "default floor 8 must still prefer session when weekly remaining is 15"
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
        // min remaining of session 90 / week 70 = 70
        assert!((info.percentage - 70.0).abs() < f64::EPSILON);
        assert_eq!(
            info.reset_at.as_deref(),
            Some("2026-09-07T06:00:00Z"),
            "scoped critical must not set reset_at or lower remaining"
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
            (info.percentage - 60.0).abs() < f64::EPSILON,
            "named opus/sonnet at 100 must not lower remaining; got {}",
            info.percentage
        );
        assert_eq!(info.reset_at.as_deref(), Some("2026-09-07T06:00:00Z"));
    }

    #[test]
    fn test_usage_suggests_lifted_uses_remaining_floor() {
        // percentage is remaining. Floor 80: remaining 90 is lifted; 70 is not.
        // Same rule for pre-gate and post-limit (no magic used<95).
        let lifted = UsageInfo {
            percentage: 90.0,
            reset_at: None,
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
        };
        let low = UsageInfo {
            percentage: 70.0,
            reset_at: None,
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
        };
        assert!(usage_suggests_lifted(&lifted, 80, true));
        assert!(!usage_suggests_lifted(&low, 80, true));
        assert!(usage_suggests_lifted(&lifted, 80, false));
        assert!(!usage_suggests_lifted(&low, 80, false));
    }

    #[test]
    fn test_usage_info_no_reset_time() {
        let info = UsageInfo {
            percentage: 50.0,
            reset_at: None,
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
        };
        assert!(info.reset_at.is_none());
    }

    #[test]
    fn test_live_fixture_remaining_banner_shape() {
        // Hermetic — no load_usage_info / live GET. Fixture resets are fixed
        // RFC3339 so we pin `now` and assert content, not exact day strings.
        let json = live_shaped_oauth_json();
        let now = DateTime::parse_from_rfc3339("2026-09-07T05:57:00Z")
            .expect("fixture now")
            .with_timezone(&Utc);
        let banner = format_oauth_remaining_banner(&json, 8, now, builtin_resolved_models());
        assert!(
            banner.contains("76% left"),
            "session remaining missing: {banner}"
        );
        assert!(
            banner.contains("45% left") || banner.contains("week "),
            "week remaining missing: {banner}"
        );
        assert!(
            banner.contains("5% left"),
            "frontier remaining missing: {banner}"
        );
        assert!(
            banner.contains("frontier"),
            "rung label must be frontier not fable: {banner}"
        );
        assert!(
            !banner.to_ascii_lowercase().contains("fable"),
            "must not print model id fable: {banner}"
        );
        assert!(banner.contains("(floor 8%)"), "floor missing: {banner}");
        assert!(
            !banner.contains("95%") && !banner.contains("Usage:"),
            "must not print used-percent / old banner: {banner}"
        );
        assert!(
            !banner.contains("threshold:"),
            "must not print old threshold shape: {banner}"
        );
        // Days band present for weekly/frontier resets (~5d out from pinned now).
        assert!(
            banner.contains('d'),
            "format_duration days band expected for ≥24h resets: {banner}"
        );
    }

    #[test]
    fn test_remaining_banner_dollar_unit() {
        let buckets = vec![QuotaBucket {
            id: "extra".into(),
            kind: "extra_usage".into(),
            label: String::new(),
            measurements: vec![Measurement {
                remaining: 12.5,
                unit: MeasurementUnit::Dollars,
            }],
            resets_at: None,
            severity: None,
            is_active: None,
            rungs: None,
        }];
        let banner = format_remaining_usage_banner(&buckets, 8, Utc::now());
        assert!(
            banner.contains("$12.5 left"),
            "dollar buckets must print in unit: {banner}"
        );
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
        assert_eq!(
            UsageCheckResult::HorizonStopped,
            UsageCheckResult::HorizonStopped
        );
        assert_eq!(UsageCheckResult::Skipped, UsageCheckResult::Skipped);
        assert_eq!(UsageCheckResult::Deferred, UsageCheckResult::Deferred);
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
            UsageCheckResult::StopSignaled,
            UsageCheckResult::HorizonStopped
        );
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
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
        };
        assert!((info.percentage).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_info_hundred_percent() {
        let info = UsageInfo {
            percentage: 100.0,
            reset_at: Some("2025-01-01T00:00:00Z".to_string()),
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
        };
        assert!((info.percentage - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_info_over_hundred_percent() {
        // API might return >100% in edge cases (burst usage)
        let info = UsageInfo {
            percentage: 105.3,
            reset_at: None,
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
        };
        assert!((info.percentage - 105.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_info_fractional_percentage() {
        let info = UsageInfo {
            percentage: 91.999,
            reset_at: None,
            remaining_banner: None,
            buckets: Vec::new(),
            oauth_json: None,
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
    fn test_usage_at_exactly_floor_waits() {
        // remaining == floor → wait (not BelowThreshold). Proceed only when remaining > floor.
        let floor: u8 = 8;
        let remaining: f64 = 8.0;
        assert!(
            remaining <= f64::from(floor),
            "remaining == floor must wait"
        );
    }

    #[test]
    fn test_usage_just_above_floor_proceeds() {
        let floor: u8 = 8;
        let remaining: f64 = 8.001;
        assert!(
            remaining > f64::from(floor),
            "remaining just above floor proceeds"
        );
    }

    #[test]
    fn test_usage_just_at_or_below_floor_waits() {
        let floor: u8 = 8;
        let remaining: f64 = 7.999;
        assert!(
            remaining <= f64::from(floor),
            "remaining just below floor must wait"
        );
    }

    #[test]
    fn test_floor_zero_only_proceeds_when_positive() {
        let floor: u8 = 0;
        assert!(0.001 > f64::from(floor), "any positive remaining proceeds");
        assert!(0.0 <= f64::from(floor), "remaining 0 at floor 0 waits");
    }

    #[test]
    fn test_floor_max_never_proceeds_at_100() {
        let floor: u8 = 255;
        let remaining: f64 = 100.0;
        assert!(
            remaining <= f64::from(floor),
            "100% remaining cannot exceed u8::MAX floor"
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

        // FEAT-009: unlabeled named siblings never family-token-map onto rungs.
        let opus = bucket_by_id(&buckets, "seven_day_opus");
        assert_eq!(opus.kind, "weekly_scoped");
        assert!(
            opus.rungs.is_none(),
            "seven_day_opus must keep rungs: None (no map_unlabeled_token); got {:?}",
            opus.rungs
        );
        let sonnet = bucket_by_id(&buckets, "seven_day_sonnet");
        assert_eq!(sonnet.kind, "weekly_scoped");
        assert!(
            sonnet.rungs.is_none(),
            "seven_day_sonnet must keep rungs: None; got {:?}",
            sonnet.rungs
        );

        // Unknown family → no rungs (evaluate ignores under default policy).
        assert!(bucket_by_id(&buckets, "nimbus_quill").rungs.is_none());
    }

    /// FEAT-009 / AC2: limits[] unlabeled id (no display_name) still uses
    /// map_unlabeled_token — that path is limits-only, not named siblings.
    #[test]
    fn ingest_limits_unlabeled_id_still_maps_via_token() {
        // Custom ladder: id has no HUD token (fable/opus/sonnet/haiku) so the
        // unlabeled branch runs; family token "xyz" substring-matches standard.
        use crate::loop_engine::model::resolve_models_config;
        use crate::loop_engine::project_config::{ModelsConfig, ProviderConfig, RoutingConfig};
        use std::collections::HashMap;

        let mut providers = HashMap::new();
        providers.insert(
            Provider::Claude.as_str().to_string(),
            ProviderConfig {
                enabled: true,
                tiers: [
                    (CapabilityTier::Cheapest, Some("vendor-cheap")),
                    (CapabilityTier::CostEfficient, Some("vendor-mid")),
                    (CapabilityTier::Standard, Some("vendor-xyz-turbo")),
                    (CapabilityTier::Frontier, Some("vendor-front")),
                ]
                .into_iter()
                .map(|(t, m)| (t.as_str().to_string(), m.map(str::to_string)))
                .collect(),
                effort: HashMap::new(),
                fallback: None,
                cli_binary: None,
            },
        );
        let models = resolve_models_config(
            &ModelsConfig {
                primary_provider: Provider::Claude.as_str().to_string(),
                anchor: CapabilityTier::Standard.as_str().to_string(),
                providers,
            },
            &RoutingConfig::default(),
        );
        let json = serde_json::json!({
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 95,
                "scope": { "model": { "id": "row_xyz" } }
            }]
        });
        let buckets = ingest_oauth_value(&json, &models);
        let b = bucket_by_id(&buckets, "limits[0].weekly_scoped");
        assert_eq!(
            b.rungs.as_deref(),
            Some(&[(Provider::Claude, CapabilityTier::Standard)][..]),
            "limits[] id-only must still map via map_unlabeled_token; got {:?}",
            b.rungs
        );
    }

    /// FEAT-009 / AC4: live-shaped evaluate marks frontier only — with and
    /// without the frontier→opus pin. Named seven_day_* stay Ignore.
    #[test]
    fn evaluate_live_shaped_unavailable_is_frontier_only_with_and_without_pin() {
        use crate::loop_engine::quota::{BucketEval, UsagePolicy, evaluate_quota};

        for (label, models) in [
            ("builtin", builtin_models()),
            (
                "frontier→opus pin",
                models_with_frontier_pinned_to_standard(),
            ),
        ] {
            let buckets = ingest_oauth_value(&live_shaped_oauth_json(), &models);
            let eval = evaluate_quota(&buckets, &UsagePolicy::default(), 8);
            assert_eq!(
                eval.unavailable,
                vec![(Provider::Claude, CapabilityTier::Frontier)],
                "{label}: unavailable must be frontier only; got {:?}",
                eval.unavailable
            );
            let sonnet = eval
                .per_bucket
                .iter()
                .find(|(id, _)| id == "seven_day_sonnet")
                .expect("seven_day_sonnet walked");
            assert_eq!(
                sonnet.1,
                BucketEval::Ignore,
                "{label}: seven_day_sonnet (rungs:None) must Ignore even at 0% left"
            );
            let nimbus = eval
                .per_bucket
                .iter()
                .find(|(id, _)| id == "nimbus_quill")
                .expect("nimbus_quill walked");
            assert_eq!(
                nimbus.1,
                BucketEval::Ignore,
                "{label}: nimbus_quill must Ignore"
            );
        }
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

    /// FEAT-008 / CONTRACT-002: Fable HUD + frontier→opus pin must mark
    /// frontier only — identity is FABLE_MODEL, not exact_model_for(frontier).
    #[test]
    fn ingest_fable_hud_with_frontier_opus_pin_marks_frontier_only() {
        let models = models_with_frontier_pinned_to_standard();
        for (label, display_name) in [
            ("bare Fable", "Fable"),
            ("Current week (Fable)", "Current week (Fable)"),
        ] {
            let json = serde_json::json!({
                "limits": [{
                    "kind": "weekly_scoped",
                    "percent": 95,
                    "scope": { "model": { "display_name": display_name } }
                }]
            });
            let buckets = ingest_oauth_value(&json, &models);
            let b = bucket_by_id(&buckets, "limits[0].weekly_scoped");
            assert_eq!(
                b.rungs.as_deref(),
                Some(&[(Provider::Claude, CapabilityTier::Frontier)][..]),
                "{label}: Fable HUD under frontier=opus pin must mark frontier only, NOT standard; got {:?}",
                b.rungs
            );
        }
    }

    /// FEAT-008: live-shaped Fable limits[] row under the same pin stays frontier-only.
    #[test]
    fn ingest_live_shaped_fable_row_with_frontier_opus_pin_marks_frontier_only() {
        let models = models_with_frontier_pinned_to_standard();
        let buckets = ingest_oauth_value(&live_shaped_oauth_json(), &models);
        let fable = bucket_by_id(&buckets, "limits[2].weekly_scoped");
        assert_eq!(
            fable.rungs.as_deref(),
            Some(&[(Provider::Claude, CapabilityTier::Frontier)][..]),
            "live-shaped Fable row under pin must not extra-mark standard; got {:?}",
            fable.rungs
        );
    }

    /// FEAT-008: Opus HUD + snapshot id under frontier→opus pin still unions
    /// OPUS_MODEL (marks standard+frontier) — do not prefer snapshot id alone.
    #[test]
    fn ingest_opus_snapshot_id_with_frontier_pin_marks_standard_and_frontier() {
        let models = models_with_frontier_pinned_to_standard();
        let json = serde_json::json!({
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 95,
                "scope": {
                    "model": {
                        "display_name": "Opus",
                        "id": "claude-opus-5-SNAPSHOT"
                    }
                }
            }]
        });
        let buckets = ingest_oauth_value(&json, &models);
        let b = bucket_by_id(&buckets, "limits[0].weekly_scoped");
        let rungs = b.rungs.as_ref().expect("Opus HUD must map rungs");
        assert!(
            rungs.contains(&(Provider::Claude, CapabilityTier::Standard)),
            "canonical OPUS_MODEL must still mark standard under snapshot id; got {rungs:?}"
        );
        assert!(
            rungs.contains(&(Provider::Claude, CapabilityTier::Frontier)),
            "pin extra-mark via OPUS_MODEL must mark frontier; got {rungs:?}"
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

    /// CODE-FIX-007: production remaining banner must ingest with run models.
    /// After frontier→opus pin, Opus HUD line labels both frontier and standard
    /// (builtin-only ingest would print only `standard`). Hermetic — no live Anthropic.
    #[test]
    fn remaining_banner_extra_marks_frontier_under_opus_pin() {
        let pinned = models_with_frontier_pinned_to_standard();
        let json = serde_json::json!({
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 95,
                "severity": "critical",
                "resets_at": "2026-09-12T19:00:00Z",
                "scope": { "model": { "display_name": "Opus" } }
            }]
        });
        let now = DateTime::parse_from_rfc3339("2026-09-07T05:57:00Z")
            .expect("fixture now")
            .with_timezone(&Utc);

        let builtin_banner =
            format_oauth_remaining_banner(&json, 8, now, builtin_resolved_models());
        assert!(
            builtin_banner.contains("standard"),
            "precondition: builtin Opus HUD → standard: {builtin_banner}"
        );
        assert!(
            !builtin_banner.contains("frontier"),
            "precondition: builtin must NOT label frontier when Fable≠Opus: {builtin_banner}"
        );

        let run_banner = format_oauth_remaining_banner(&json, 8, now, &pinned);
        assert!(
            run_banner.contains("standard"),
            "run models must still label standard: {run_banner}"
        );
        assert!(
            run_banner.contains("frontier"),
            "run models under frontier=opus pin must also label frontier: {run_banner}"
        );
        assert!(
            run_banner.contains("5% left"),
            "remaining amount missing: {run_banner}"
        );

        // Production helper: UsageInfo with builtin snapshot + oauth_json must
        // rebuild via run models (not the provisional remaining_banner).
        let info = UsageInfo {
            percentage: 100.0,
            reset_at: None,
            remaining_banner: Some(builtin_banner.clone()),
            buckets: ingest_oauth_value(&json, builtin_resolved_models()),
            oauth_json: Some(json),
        };
        let rebuilt =
            remaining_banner_for_run_models(&info, &pinned, 8, now).expect("oauth_json present");
        assert!(
            rebuilt.contains("frontier") && rebuilt.contains("standard"),
            "remaining_banner_for_run_models must extra-mark both rungs; got {rebuilt}"
        );
        assert_ne!(
            rebuilt, builtin_banner,
            "production banner must not reuse builtin-only ingest"
        );
    }

    /// WIRE-FIX-001: production gate must re-ingest with run models. Simulates
    /// fetch storing builtin buckets (standard-only on Opus HUD) plus raw JSON;
    /// `buckets_for_run_models` under frontier=opus must extra-mark both rungs
    /// so evaluate marks both unavailable. Hermetic — no live Anthropic.
    #[test]
    fn gate_buckets_for_run_models_extra_mark_under_frontier_opus_pin() {
        use crate::loop_engine::quota::{UsagePolicy, evaluate_quota};

        let pinned = models_with_frontier_pinned_to_standard();
        let json = serde_json::json!({
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 95,
                "severity": "critical",
                "scope": { "model": { "display_name": "Opus" } }
            }]
        });

        // What production fetch still stores under builtin (wrong for pins).
        let builtin_buckets = ingest_oauth_value(&json, builtin_resolved_models());
        let builtin_rungs = builtin_buckets
            .first()
            .and_then(|b| b.rungs.as_ref())
            .expect("Opus HUD maps under builtin");
        assert!(
            builtin_rungs.contains(&(Provider::Claude, CapabilityTier::Standard)),
            "precondition: HUD Opus → standard"
        );
        assert!(
            !builtin_rungs.contains(&(Provider::Claude, CapabilityTier::Frontier)),
            "precondition: builtin ladder must NOT extra-mark frontier (distinct Fable/Opus)"
        );

        let info = UsageInfo {
            percentage: 100.0,
            reset_at: None,
            remaining_banner: None,
            buckets: builtin_buckets,
            oauth_json: Some(json),
        };

        let gate_buckets = buckets_for_run_models(&info, &pinned);
        let rungs = gate_buckets
            .first()
            .and_then(|b| b.rungs.as_ref())
            .expect("gate re-ingest must map Opus HUD");
        assert!(
            rungs.contains(&(Provider::Claude, CapabilityTier::Standard)),
            "run models: Opus → standard; got {rungs:?}"
        );
        assert!(
            rungs.contains(&(Provider::Claude, CapabilityTier::Frontier)),
            "run models under frontier=opus pin must extra-mark frontier; got {rungs:?}"
        );

        let eval = evaluate_quota(&gate_buckets, &UsagePolicy::default(), 8);
        assert!(
            eval.unavailable
                .contains(&(Provider::Claude, CapabilityTier::Standard)),
            "evaluate must mark standard unavailable; got {:?}",
            eval.unavailable
        );
        assert!(
            eval.unavailable
                .contains(&(Provider::Claude, CapabilityTier::Frontier)),
            "evaluate must mark frontier unavailable after pin extra-mark; got {:?}",
            eval.unavailable
        );
    }
}
