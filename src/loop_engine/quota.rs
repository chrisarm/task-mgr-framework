//! Generic quota buckets + pure evaluate classifier (PR-2 / FR-003).
//!
//! Engine language is capability rungs only — never model-id or HUD-label
//! literals. Display-name → rung mapping lives in the `usage` ingest adapter.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::loop_engine::model::{CapabilityTier, Provider};

/// Unit of a remaining measurement. Percent is the gate unit (0–100, never a 0.08 ratio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementUnit {
    Percent,
    Dollars,
    Tokens,
    Credits,
}

/// One remaining-first measurement on a bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    /// Remaining quantity. For `Percent`: **0.0–100.0** (never 0.08).
    pub remaining: f64,
    pub unit: MeasurementUnit,
}

/// One ingested API window / limits[] row.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaBucket {
    /// Stable id: named key (`"five_hour"`) or synthetic from limits[] index/kind.
    pub id: String,
    /// API kind string: `"session"` | `"weekly_all"` | `"weekly_scoped"` | spend/extra/… .
    /// Named object siblings without `kind` map: `five_hour`→session, `seven_day`→weekly_all;
    /// other named weekly siblings that carry rungs are rung-scoped (not account-binding).
    pub kind: String,
    /// Human label (HUD / stderr); may be empty.
    pub label: String,
    /// Remaining-first. Percent measurement preferred when present.
    pub measurements: Vec<Measurement>,
    /// RFC3339 reset timestamp when known.
    pub resets_at: Option<String>,
    /// Display hint only — MUST NOT drive default-low unless a rule `when` opts in.
    pub severity: Option<String>,
    /// Display hint only — MUST NOT drive default-low unless a rule `when` opts in.
    pub is_active: Option<bool>,
    /// `None` / empty = account-binding candidate (session / weekly_all).
    /// `Some(non-empty)` = rung-scoped; keys are capability rungs, never model ids.
    pub rungs: Option<Vec<(Provider, CapabilityTier)>>,
}

/// Ordered match rule. First match wins. Explicit `onLow` overrides the horizon heuristic.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Opt-in predicate (e.g. severity). Absent = default remaining-floor low only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<serde_json::Value>,
    pub on_low: OnLowAction,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OnLowAction {
    Wait,
    Unavailable,
    Stop,
    Ask,
    Ignore,
}

/// Per-kind remaining-percent floors (0–100, never a 0.08 ratio).
///
/// Factory: **other** (session / extra / …) **2**, **weekly** (`weekly_all` /
/// `weekly_scoped`) **1**. That leaves a thin reserve for wrap-up / a couple
/// of manual session turns after the loop parks. Classifier tests that pin a
/// historical bar use [`RemainingFloors::uniform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemainingFloors {
    /// Session and other non-weekly percent windows.
    pub other: u8,
    /// `weekly_all` / `weekly_scoped` percent windows.
    pub weekly: u8,
}

impl RemainingFloors {
    /// Same floor on every percent window (hermetic classifier tests).
    pub const fn uniform(n: u8) -> Self {
        Self {
            other: n,
            weekly: n,
        }
    }

    /// Factory defaults: other 2, weekly 1.
    pub const fn factory() -> Self {
        Self {
            other: 2,
            weekly: 1,
        }
    }

    /// Floor for an ingested bucket `kind`.
    pub fn for_kind(self, kind: &str) -> u8 {
        if is_weekly_kind(kind) {
            self.weekly
        } else {
            self.other
        }
    }

    /// Operator banner suffix. Uniform → `(floor N%)`; split →
    /// `(floor 2% · weekly 1%)`.
    pub fn banner_suffix(self) -> String {
        if self.other == self.weekly {
            format!("(floor {}%)", self.other)
        } else {
            format!("(floor {}% · weekly {}%)", self.other, self.weekly)
        }
    }
}

impl Default for RemainingFloors {
    fn default() -> Self {
        Self::factory()
    }
}

/// Weekly percent windows that use [`RemainingFloors::weekly`].
pub fn is_weekly_kind(kind: &str) -> bool {
    matches!(kind, "weekly_all" | "weekly_scoped")
}

/// Operator usage policy (remaining floor + horizon knobs + ordered rules).
///
/// Serde wiring onto `routing` / config JSON lands with FEAT-008; evaluate takes
/// this by reference so FEAT-003 stays free of clap / project_config I/O.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsagePolicy {
    /// Remaining-percent floor for session / other percent windows (0–100).
    /// Default **2**. Not a ratio. Weekly windows use
    /// [`Self::remaining_min_weekly_percent`].
    #[serde(default = "default_remaining_min_percent")]
    pub remaining_min_percent: u8,
    /// Remaining-percent floor for weekly windows (0–100). Default **1**.
    #[serde(default = "default_remaining_min_weekly_percent")]
    pub remaining_min_weekly_percent: u8,
    #[serde(default = "default_wait_if_reset_within_minutes")]
    pub wait_if_reset_within_minutes: u64,
    #[serde(default = "default_stop_if_reset_beyond_hours")]
    pub stop_if_reset_beyond_hours: u64,
    /// Ask TTL minutes. Default **0**. Clap `--use-other-models-ttl` is PR-3.
    #[serde(default = "default_ask_ttl_minutes")]
    pub ask_ttl_minutes: u64,
    #[serde(default)]
    pub rules: Vec<UsageRule>,
}

fn default_remaining_min_percent() -> u8 {
    2
}
fn default_remaining_min_weekly_percent() -> u8 {
    1
}
fn default_wait_if_reset_within_minutes() -> u64 {
    60
}
fn default_stop_if_reset_beyond_hours() -> u64 {
    12
}
fn default_ask_ttl_minutes() -> u64 {
    0
}

impl Default for UsagePolicy {
    fn default() -> Self {
        Self {
            remaining_min_percent: default_remaining_min_percent(),
            remaining_min_weekly_percent: default_remaining_min_weekly_percent(),
            wait_if_reset_within_minutes: default_wait_if_reset_within_minutes(),
            stop_if_reset_beyond_hours: default_stop_if_reset_beyond_hours(),
            ask_ttl_minutes: default_ask_ttl_minutes(),
            rules: Vec::new(),
        }
    }
}

impl UsagePolicy {
    /// Overlay per-run CLI horizon flags. `None` keeps the current value
    /// (`Some(0)` is present zero — wait-within 0 minutes).
    pub fn overlay_horizon(
        &self,
        wait_within_minutes: Option<u64>,
        stop_beyond_hours: Option<u64>,
    ) -> Self {
        let mut out = self.clone();
        if let Some(w) = wait_within_minutes {
            out.wait_if_reset_within_minutes = w;
        }
        if let Some(s) = stop_beyond_hours {
            out.stop_if_reset_beyond_hours = s;
        }
        out
    }

    /// Floors from this policy's serde fields (before CLI/env overlay).
    pub fn floors(&self) -> RemainingFloors {
        RemainingFloors {
            other: self.remaining_min_percent,
            weekly: self.remaining_min_weekly_percent,
        }
    }
}

/// Per-bucket fact from evaluate. **Never** includes Ask.
#[derive(Debug, Clone, PartialEq)]
pub enum BucketEval {
    /// Remaining above floor (and no matching opt-in when), or explicit onLow: ignore,
    /// or unknown/non-actionable bucket (e.g. nimbus_quill default).
    Ignore,
    /// Rung-scoped low under the heuristic (or explicit onLow: unavailable).
    /// Rungs to mark unavailable. Explicit wait|stop|ask do **not** collapse here.
    Unavailable {
        rungs: Vec<(Provider, CapabilityTier)>,
    },
    /// Wait/stop/ask **inputs** (account-binding, amount-exhausted spend, or
    /// explicit onLow wait|stop|ask on any bucket including weekly_scoped).
    /// Apply resolves Wait vs Stop vs Ask. Evaluate does not emit Ask.
    AccountLow {
        remaining: f64,
        reset_secs: Option<u64>,
        kind: String,
        low: bool,
    },
}

/// Aggregated evaluate output. No Ask variant anywhere.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuotaEval {
    pub per_bucket: Vec<(String, BucketEval)>,
    /// Deduped unavailable rungs across buckets (proto-channel source).
    pub unavailable: Vec<(Provider, CapabilityTier)>,
    /// Account wait/stop inputs (may coexist with unavailable).
    pub account_low: Vec<AccountLowInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountLowInput {
    pub bucket_id: String,
    pub remaining: f64,
    pub reset_secs: Option<u64>,
    pub kind: String,
    pub low: bool,
}

/// Pure. Sub-millisecond. No I/O.
/// Does **NOT** take `other_rungs_runnable: bool`.
/// Does **NOT** emit ask.
pub fn evaluate_quota(
    buckets: &[QuotaBucket],
    policy: &UsagePolicy,
    floors: RemainingFloors,
) -> QuotaEval {
    let now = Utc::now();
    let mut per_bucket = Vec::with_capacity(buckets.len());
    let mut unavailable: Vec<(Provider, CapabilityTier)> = Vec::new();
    let mut account_low = Vec::new();

    for bucket in buckets {
        let eval = evaluate_one(bucket, policy, floors, now);
        match &eval {
            BucketEval::Unavailable { rungs } => {
                for rung in rungs {
                    if !unavailable.contains(rung) {
                        unavailable.push(*rung);
                    }
                }
            }
            BucketEval::AccountLow {
                remaining,
                reset_secs,
                kind,
                low,
            } => {
                account_low.push(AccountLowInput {
                    bucket_id: bucket.id.clone(),
                    remaining: *remaining,
                    reset_secs: *reset_secs,
                    kind: kind.clone(),
                    low: *low,
                });
            }
            BucketEval::Ignore => {}
        }
        per_bucket.push((bucket.id.clone(), eval));
    }

    QuotaEval {
        per_bucket,
        unavailable,
        account_low,
    }
}

fn evaluate_one(
    bucket: &QuotaBucket,
    policy: &UsagePolicy,
    floors: RemainingFloors,
    now: DateTime<Utc>,
) -> BucketEval {
    let floor = f64::from(floors.for_kind(&bucket.kind));
    let percent = percent_remaining(bucket);
    let amount = non_percent_remaining(bucket);
    let rung_scoped = is_rung_scoped(bucket);
    let account_binding = is_account_binding(bucket);

    // Default low: percent remaining ≤ floor. severity / is_active are NOT low.
    // Dollars/tokens/credits: never use the percent floor; amount ≤ 0 may surface
    // as AccountLow (apply stops only at ≤ 0).
    let percent_low = percent.is_some_and(|r| r <= floor);
    let amount_exhausted = amount.is_some_and(|(r, _)| r <= 0.0);

    let rule = first_matching_rule(policy, bucket);

    // Explicit onLow ignore — even when low.
    if let Some(rule) = rule
        && matches!(rule.on_low, OnLowAction::Ignore)
    {
        // Still only applies when the bucket is "low" under default or when-opt-in,
        // OR when the rule's `when` already matched (first_matching_rule).
        // Contract: explicit rule onLow ignore is applied here.
        if percent_low || amount_exhausted || rule.when.is_some() {
            return BucketEval::Ignore;
        }
    }

    if !percent_low && !amount_exhausted {
        return BucketEval::Ignore;
    }

    // Prefer percent remaining for AccountLow payload when present.
    let remaining_value = percent.unwrap_or_else(|| amount.map(|(r, _)| r).unwrap_or(0.0));
    let reset_secs = reset_secs_until(&bucket.resets_at, now);

    if let Some(rule) = rule {
        match rule.on_low {
            OnLowAction::Ignore => return BucketEval::Ignore,
            OnLowAction::Unavailable => {
                if rung_scoped {
                    return BucketEval::Unavailable {
                        rungs: bucket.rungs.clone().unwrap_or_default(),
                    };
                }
                // Explicit unavailable on a non-rung bucket: no rungs to mark.
                return BucketEval::Ignore;
            }
            // Explicit wait|stop|ask wins over the scoped→Unavailable heuristic.
            // Emit AccountLow so apply can honor the action (never Ask here).
            OnLowAction::Wait | OnLowAction::Stop | OnLowAction::Ask => {
                return BucketEval::AccountLow {
                    remaining: remaining_value,
                    reset_secs,
                    kind: bucket.kind.clone(),
                    low: true,
                };
            }
        }
    }

    // Default (no matching rule):
    if rung_scoped && percent_low {
        return BucketEval::Unavailable {
            rungs: bucket.rungs.clone().unwrap_or_default(),
        };
    }
    if account_binding && percent_low {
        return BucketEval::AccountLow {
            remaining: remaining_value,
            reset_secs,
            kind: bucket.kind.clone(),
            low: true,
        };
    }
    // Ignore promotional / extra / unknown amount buckets BEFORE amount_exhausted
    // AccountLow. Dropping them from apply's is_spend_kind alone is not enough —
    // account_low_is_amount_only would still Stop. AccountLow at amount ≤ 0 is
    // reserved for spend / credits / dollars / tokens only.
    if matches!(
        bucket.kind.as_str(),
        "extra_usage" | "promotional" | "nimbus_quill"
    ) {
        return BucketEval::Ignore;
    }
    if amount_exhausted && !rung_scoped {
        // Spend / dollars / credits / tokens exhausted → account-low (apply stops).
        return BucketEval::AccountLow {
            remaining: remaining_value,
            reset_secs,
            kind: bucket.kind.clone(),
            low: true,
        };
    }

    // Unknown kind with no rungs → ignore.
    BucketEval::Ignore
}

fn percent_remaining(bucket: &QuotaBucket) -> Option<f64> {
    bucket
        .measurements
        .iter()
        .find(|m| m.unit == MeasurementUnit::Percent)
        .map(|m| m.remaining)
}

fn non_percent_remaining(bucket: &QuotaBucket) -> Option<(f64, MeasurementUnit)> {
    bucket
        .measurements
        .iter()
        .find(|m| m.unit != MeasurementUnit::Percent)
        .map(|m| (m.remaining, m.unit))
}

fn is_rung_scoped(bucket: &QuotaBucket) -> bool {
    bucket.rungs.as_ref().is_some_and(|r| !r.is_empty())
}

fn is_account_binding(bucket: &QuotaBucket) -> bool {
    !is_rung_scoped(bucket) && (bucket.kind == "session" || bucket.kind == "weekly_all")
}

fn first_matching_rule<'a>(policy: &'a UsagePolicy, bucket: &QuotaBucket) -> Option<&'a UsageRule> {
    policy.rules.iter().find(|rule| rule_matches(rule, bucket))
}

fn rule_matches(rule: &UsageRule, bucket: &QuotaBucket) -> bool {
    if let Some(want_kind) = rule.kind.as_ref()
        && want_kind != &bucket.kind
    {
        return false;
    }
    if let Some(want_id) = rule.id.as_ref()
        && want_id != &bucket.id
    {
        return false;
    }
    if let Some(when) = rule.when.as_ref() {
        if let Some(want_sev) = when.get("severity").and_then(|v| v.as_str()) {
            if bucket.severity.as_deref() != Some(want_sev) {
                return false;
            }
        } else {
            // Unknown / empty when predicate — do not match.
            return false;
        }
    }
    // A rule with no kind/id/when would match everything; allow it (catch-all).
    true
}

fn reset_secs_until(resets_at: &Option<String>, now: DateTime<Utc>) -> Option<u64> {
    let raw = resets_at.as_deref()?;
    let ts = DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc())
        })?;
    let delta = ts.signed_duration_since(now).num_seconds();
    Some(if delta <= 0 { 0 } else { delta as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn percent_bucket(
        id: &str,
        kind: &str,
        remaining: f64,
        rungs: Option<Vec<(Provider, CapabilityTier)>>,
    ) -> QuotaBucket {
        QuotaBucket {
            id: id.into(),
            kind: kind.into(),
            label: String::new(),
            measurements: vec![Measurement {
                remaining,
                unit: MeasurementUnit::Percent,
            }],
            resets_at: Some("2099-01-01T00:00:00Z".into()),
            severity: Some("critical".into()),
            is_active: Some(true),
            rungs,
        }
    }

    #[test]
    fn evaluate_ignores_when_remaining_above_floor() {
        let buckets = [percent_bucket("five_hour", "session", 76.0, None)];
        let eval = evaluate_quota(
            &buckets,
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        assert_eq!(eval.per_bucket.len(), 1);
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        assert!(eval.unavailable.is_empty());
        assert!(eval.account_low.is_empty());
    }

    #[test]
    fn evaluate_account_binding_low_emits_account_low_not_ask() {
        let buckets = [percent_bucket("seven_day", "weekly_all", 5.0, None)];
        let eval = evaluate_quota(
            &buckets,
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        match &eval.per_bucket[0].1 {
            BucketEval::AccountLow {
                remaining,
                kind,
                low,
                reset_secs,
            } => {
                assert!((*remaining - 5.0).abs() < f64::EPSILON);
                assert_eq!(kind, "weekly_all");
                assert!(*low);
                assert!(reset_secs.is_some_and(|s| s > 0));
            }
            other => panic!("expected AccountLow, got {other:?}"),
        }
        assert_eq!(eval.account_low.len(), 1);
        assert!(eval.unavailable.is_empty());
        // No Ask variant exists on BucketEval / QuotaEval.
    }

    #[test]
    fn evaluate_rung_scoped_low_emits_unavailable() {
        let rungs = vec![
            (Provider::Claude, CapabilityTier::Frontier),
            (Provider::Claude, CapabilityTier::Standard),
        ];
        let buckets = [percent_bucket(
            "limits[2].weekly_scoped",
            "weekly_scoped",
            5.0,
            Some(rungs.clone()),
        )];
        let eval = evaluate_quota(
            &buckets,
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        match &eval.per_bucket[0].1 {
            BucketEval::Unavailable { rungs: got } => assert_eq!(got, &rungs),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert_eq!(eval.unavailable, rungs);
        assert!(
            eval.account_low.is_empty(),
            "rung-scoped low must not account-wait in evaluate"
        );
    }

    #[test]
    fn evaluate_severity_critical_is_not_default_low() {
        // Remaining 50 > floor 8; severity=critical must NOT force low.
        let mut b = percent_bucket("weekly", "weekly_all", 50.0, None);
        b.severity = Some("critical".into());
        b.is_active = Some(true);
        let eval = evaluate_quota(&[b], &UsagePolicy::default(), RemainingFloors::uniform(8));
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
    }

    #[test]
    fn evaluate_unknown_kind_low_is_ignore() {
        // nimbus_quill-shaped: 0% remaining, no rungs, not session/weekly_all.
        let buckets = [percent_bucket("nimbus_quill", "nimbus_quill", 0.0, None)];
        let eval = evaluate_quota(
            &buckets,
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        assert!(eval.account_low.is_empty());
        assert!(eval.unavailable.is_empty());
    }

    #[test]
    fn evaluate_explicit_on_low_ignore() {
        let policy = UsagePolicy {
            rules: vec![UsageRule {
                kind: Some("weekly_scoped".into()),
                id: None,
                when: None,
                on_low: OnLowAction::Ignore,
            }],
            ..UsagePolicy::default()
        };
        let buckets = [percent_bucket(
            "scoped",
            "weekly_scoped",
            0.0,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        )];
        let eval = evaluate_quota(&buckets, &policy, RemainingFloors::uniform(8));
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        assert!(eval.unavailable.is_empty());
    }

    #[test]
    fn evaluate_explicit_on_low_wait_on_weekly_scoped_emits_account_low() {
        // CODE-FIX-006: explicit wait must not collapse to Unavailable-only.
        let policy = UsagePolicy {
            rules: vec![UsageRule {
                kind: Some("weekly_scoped".into()),
                id: None,
                when: None,
                on_low: OnLowAction::Wait,
            }],
            ..UsagePolicy::default()
        };
        let buckets = [percent_bucket(
            "limits[2].weekly_scoped",
            "weekly_scoped",
            5.0,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        )];
        let eval = evaluate_quota(&buckets, &policy, RemainingFloors::uniform(8));
        match &eval.per_bucket[0].1 {
            BucketEval::AccountLow {
                remaining,
                kind,
                low,
                reset_secs,
            } => {
                assert!((*remaining - 5.0).abs() < f64::EPSILON);
                assert_eq!(kind, "weekly_scoped");
                assert!(*low);
                assert!(reset_secs.is_some_and(|s| s > 0));
            }
            other => panic!("expected AccountLow wait input, got {other:?}"),
        }
        assert_eq!(eval.account_low.len(), 1);
        assert!(
            eval.unavailable.is_empty(),
            "explicit wait must not mark rungs unavailable (apply honors wait)"
        );
    }

    #[test]
    fn evaluate_explicit_on_low_stop_and_ask_on_scoped_emit_account_low() {
        for action in [OnLowAction::Stop, OnLowAction::Ask] {
            let policy = UsagePolicy {
                rules: vec![UsageRule {
                    kind: Some("weekly_scoped".into()),
                    id: None,
                    when: None,
                    on_low: action,
                }],
                ..UsagePolicy::default()
            };
            let buckets = [percent_bucket(
                "scoped",
                "weekly_scoped",
                0.0,
                Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
            )];
            let eval = evaluate_quota(&buckets, &policy, RemainingFloors::uniform(8));
            assert!(
                matches!(eval.per_bucket[0].1, BucketEval::AccountLow { .. }),
                "{action:?} on scoped must emit AccountLow, got {:?}",
                eval.per_bucket[0].1
            );
            assert_eq!(eval.account_low.len(), 1);
            assert!(eval.unavailable.is_empty());
        }
    }

    #[test]
    fn evaluate_spend_amount_positive_ignores_percent_floor() {
        // Dollars-only, remaining $5 — must NOT be low at percent floor.
        let bucket = QuotaBucket {
            id: "spend".into(),
            kind: "spend".into(),
            label: String::new(),
            measurements: vec![Measurement {
                remaining: 5.0,
                unit: MeasurementUnit::Dollars,
            }],
            resets_at: None,
            severity: None,
            is_active: None,
            rungs: None,
        };
        let eval = evaluate_quota(
            &[bucket],
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
    }

    #[test]
    fn evaluate_spend_amount_zero_emits_account_low() {
        let bucket = QuotaBucket {
            id: "spend".into(),
            kind: "spend".into(),
            label: String::new(),
            measurements: vec![Measurement {
                remaining: 0.0,
                unit: MeasurementUnit::Dollars,
            }],
            resets_at: None,
            severity: None,
            is_active: None,
            rungs: None,
        };
        let eval = evaluate_quota(
            &[bucket],
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        match &eval.per_bucket[0].1 {
            BucketEval::AccountLow { remaining, low, .. } => {
                assert!(*remaining == 0.0);
                assert!(*low);
            }
            other => panic!("expected AccountLow for exhausted spend, got {other:?}"),
        }
    }

    fn dollars_bucket(id: &str, kind: &str, remaining: f64) -> QuotaBucket {
        QuotaBucket {
            id: id.into(),
            kind: kind.into(),
            label: String::new(),
            measurements: vec![Measurement {
                remaining,
                unit: MeasurementUnit::Dollars,
            }],
            resets_at: None,
            severity: None,
            is_active: None,
            rungs: None,
        }
    }

    #[test]
    fn evaluate_extra_usage_dollars_zero_is_ignore() {
        let eval = evaluate_quota(
            &[dollars_bucket("extra_usage", "extra_usage", 0.0)],
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        assert!(eval.account_low.is_empty());
    }

    #[test]
    fn evaluate_nimbus_quill_dollars_zero_is_ignore() {
        let eval = evaluate_quota(
            &[dollars_bucket("nimbus_quill", "nimbus_quill", 0.0)],
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        assert!(eval.account_low.is_empty());
    }

    #[test]
    fn evaluate_promotional_dollars_zero_is_ignore() {
        let eval = evaluate_quota(
            &[dollars_bucket("promotional", "promotional", 0.0)],
            &UsagePolicy::default(),
            RemainingFloors::uniform(8),
        );
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        assert!(eval.account_low.is_empty());
    }

    #[test]
    fn evaluate_does_not_take_other_rungs_runnable() {
        // Compile-time / API shape guard: three-arg signature only.
        let _f: fn(&[QuotaBucket], &UsagePolicy, RemainingFloors) -> QuotaEval = evaluate_quota;
    }

    #[test]
    fn factory_floors_weekly_two_is_ignore_one_is_low() {
        let policy = UsagePolicy::default();
        let floors = RemainingFloors::factory();
        let above = percent_bucket("seven_day", "weekly_all", 2.0, None);
        let eval = evaluate_quota(std::slice::from_ref(&above), &policy, floors);
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        let low = percent_bucket("seven_day", "weekly_all", 1.0, None);
        let eval = evaluate_quota(std::slice::from_ref(&low), &policy, floors);
        match &eval.per_bucket[0].1 {
            BucketEval::AccountLow {
                remaining,
                kind,
                low,
                ..
            } => {
                assert!((*remaining - 1.0).abs() < f64::EPSILON);
                assert_eq!(kind, "weekly_all");
                assert!(*low);
            }
            other => panic!("expected AccountLow, got {other:?}"),
        }
    }

    #[test]
    fn factory_floors_session_three_is_ignore_two_is_low() {
        let policy = UsagePolicy::default();
        let floors = RemainingFloors::factory();
        let above = percent_bucket("five_hour", "session", 3.0, None);
        let eval = evaluate_quota(std::slice::from_ref(&above), &policy, floors);
        assert_eq!(eval.per_bucket[0].1, BucketEval::Ignore);
        let low = percent_bucket("five_hour", "session", 2.0, None);
        let eval = evaluate_quota(std::slice::from_ref(&low), &policy, floors);
        match &eval.per_bucket[0].1 {
            BucketEval::AccountLow {
                remaining,
                kind,
                low,
                ..
            } => {
                assert!((*remaining - 2.0).abs() < f64::EPSILON);
                assert_eq!(kind, "session");
                assert!(*low);
            }
            other => panic!("expected AccountLow, got {other:?}"),
        }
    }

    #[test]
    fn factory_floors_session_two_weekly_two_only_session_low() {
        let policy = UsagePolicy::default();
        let floors = RemainingFloors::factory();
        let buckets = [
            percent_bucket("five_hour", "session", 2.0, None),
            percent_bucket("seven_day", "weekly_all", 2.0, None),
        ];
        let eval = evaluate_quota(&buckets, &policy, floors);
        match &eval.per_bucket[0].1 {
            BucketEval::AccountLow { kind, .. } => assert_eq!(kind, "session"),
            other => panic!("session 2 must be AccountLow, got {other:?}"),
        }
        assert_eq!(eval.per_bucket[1].1, BucketEval::Ignore);
        assert_eq!(eval.account_low.len(), 1);
        assert_eq!(eval.account_low[0].kind, "session");
    }

    #[test]
    fn overlay_horizon_present_zero_wait_is_not_omitted() {
        let policy = UsagePolicy::default();
        let over = policy.overlay_horizon(Some(0), None);
        assert_eq!(over.wait_if_reset_within_minutes, 0);
        assert_eq!(over.stop_if_reset_beyond_hours, 12);
        let over = policy.overlay_horizon(None, Some(1));
        assert_eq!(over.wait_if_reset_within_minutes, 60);
        assert_eq!(over.stop_if_reset_beyond_hours, 1);
    }

    #[test]
    fn quota_module_has_no_model_id_literals() {
        // Defense in depth alongside tests/no_hardcoded_models.rs — engine keys
        // are rungs only. Assemble forbidden tokens at runtime so this source
        // file itself stays free of them (AC grep over quota.rs/engine.rs → 0).
        let src = include_str!("quota.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        let family: String = ['e', 'l', 'b', 'a', 'f'].into_iter().rev().collect();
        let needles = [
            format!("claude-{family}-5"),
            format!("claude-{}", "opus"),
            format!("claude-{}", "sonnet"),
            format!("claude-{}", "haiku"),
            family.clone(),
        ];
        for needle in &needles {
            assert!(
                !prod.contains(needle),
                "quota.rs must not contain model-id literal {needle}"
            );
        }
    }
}
