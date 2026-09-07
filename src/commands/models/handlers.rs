//! Command handlers for the provider-first `task-mgr models` verb set (FR-009).
//!
//! Verbs:
//! - `init [--force-replace-legacy] [--dry-run]` — write the FR-001 default
//!   `models`/`routing` block. `--force-replace-legacy` is the ONE sanctioned
//!   migration: it deletes exactly the four legacy keys
//!   (`defaultModel`/`reviewModel`/`primaryRunner`/`fallbackRunner`) and writes
//!   the default block. `--dry-run` prints both halves of the diff without writing.
//! - `show` — full routing table (explicit empty states) + the anchor-derived
//!   difficulty→model mapping + the crash-escalation cost note + the codex
//!   route-only pinning note + a legacy detect-and-instruct banner.
//! - `list [--remote] [--refresh]` — reverse-lookup of the merged provider tier
//!   ladders + effort tables; `--remote` consults the Anthropic catalog.
//! - `set-anchor <tier>` — set `models.anchor`.
//! - `enable <provider>` / `disable <provider>` — flip `models.providers.<p>.enabled`.
//!   `enable` probes the provider binary BEFORE writing; `disable` never probes.
//! - `set-tier <provider> <tier> [model]` / `unset-tier <provider> <tier>` —
//!   manage `models.providers.<p>.tiers.<tier>` (omitted model = `null` = route
//!   with no model flag).
//! - `set-effort <provider> <difficulty> [effort]` — set
//!   `models.providers.<p>.effort.<difficulty>`.
//! - `set-fallback <provider> <target>` / `unset-fallback <provider>` — manage
//!   the tier-preserving cross-provider `models.providers.<p>.fallback`.
//! - `route <prefix> --provider <p> [--tier <t>]` / `unroute <prefix>` — manage
//!   `routing.byIdPrefix`.
//! - `set-usage-rule --kind/--id --on-low` — append/replace a
//!   `usagePolicy.rules` entry.
//! - `set-tier-fallback <low|medium|high> [--include-review] [--include-forced]`
//!   / `unset-tier-fallback` — write `routing.tierFallback` (unset = JSON
//!   `null`, never key deletion).
//!
//! Every mutating verb (1) hard-errors on a config still carrying legacy keys
//! (the migration is `models init --force-replace-legacy`), (2) strictly
//! validates its input (`CapabilityTier::parse` / `parse_config_provider` — a
//! typo is a CONFIG ERROR naming the accepted set), (3) round-trips the config
//! through `serde_json::Value` mutating only the targeted nested path so unknown
//! keys (`additionalAllowedTools`, `embeddingModel`, …) survive verbatim, and
//! (4) validates the would-be config through `validate_models_config` BEFORE
//! writing — an invalid mutation is rejected with the config left untouched.

use std::io;
use std::path::Path;

use super::api::{ApiError, check_opt_in, fetch_models, sort_newest_first};
use super::cache;
use crate::loop_engine::config_io::write_config_value_at;
use crate::loop_engine::model::{
    CapabilityTier, Provider, ResolvedModelsConfig, anchored_tier, difficulty_rank, escalate_tier,
    parse_config_provider, resolve_models_config,
};
use crate::loop_engine::project_config::{
    LEGACY_MODEL_KEYS, ModelsConfig, ProviderConfig, RoutingConfig, detect_legacy_model_keys,
    fr_001_default_block, legacy_model_keys_message, merge_models_config,
    probe_enabled_provider_binaries, read_project_config, validate_models_config,
};
use crate::loop_engine::quota::{OnLowAction, UsagePolicy};
use crate::loop_engine::usage::{load_usage_info, remaining_banner_for_run_models};
use crate::output::ui;
use chrono::Utc;

/// Provider display order used everywhere the routing table is rendered.
const DISPLAY_PROVIDERS: [Provider; 3] = [Provider::Claude, Provider::Grok, Provider::Codex];

/// Difficulty buckets, ascending. The valid keys for `set-effort` and the
/// anchor-window rows in `show`. SSoT for ordering is `difficulty_rank`.
const DIFFICULTIES: [&str; 3] = ["low", "medium", "high"];

/// Options for `models list`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListOpts {
    /// Consult the Anthropic API if possible (requires opt-in).
    pub remote: bool,
    /// Force cache refresh before fetching. Implies `remote`.
    pub refresh: bool,
}

// ============================================================================
// Shared config round-trip plumbing (read → mutate nested path → validate → write)
// ============================================================================

/// Read `<db_dir>/config.json` as a `serde_json::Value`, seeding `{"version":1}`
/// when the file is absent or empty. Malformed JSON is a hard error (the path is
/// named) — a mutating verb must never silently clobber a config it couldn't
/// parse.
fn read_config_value(db_dir: &Path) -> io::Result<serde_json::Value> {
    let path = db_dir.join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: malformed JSON: {e}", path.display()),
            )
        }),
        _ => Ok(serde_json::json!({ "version": 1 })),
    }
}

/// Atomically persist a whole config `Value` to `<db_dir>/config.json`.
fn write_config_value(db_dir: &Path, value: &serde_json::Value) -> io::Result<()> {
    write_config_value_at(&db_dir.join("config.json"), value)
}

/// Set (or, with `leaf = None`, remove) the value at a nested object `path`,
/// creating intermediate objects as needed. Replaces a non-object intermediate
/// with a fresh object rather than panicking — operators don't hand-build these
/// paths, so a scalar where an object belongs is recovered, not honored.
fn set_json_path(
    root: &mut serde_json::Value,
    path: &[&str],
    leaf: Option<serde_json::Value>,
) -> io::Result<()> {
    use serde_json::{Map, Value};
    let (last, parents) = path.split_last().expect("path must be non-empty");
    let mut cur = root;
    for key in parents {
        let map = cur.as_object_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "config: expected a JSON object while descending the models/routing path",
            )
        })?;
        let child = map
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !child.is_object() {
            *child = Value::Object(Map::new());
        }
        cur = child;
    }
    let map = cur.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "config: expected a JSON object at the models/routing leaf",
        )
    })?;
    match leaf {
        Some(v) => {
            map.insert(last.to_string(), v);
        }
        None => {
            map.remove(*last);
        }
    }
    Ok(())
}

/// Build the merged `(models, routing)` from a candidate config value and run
/// the pure semantic validation. Returns the typed pair on success so callers
/// (e.g. `enable`) can resolve + probe without re-merging. A validation failure
/// is a single CONFIG ERROR string joining every problem found.
fn build_and_validate(value: &serde_json::Value) -> Result<(ModelsConfig, RoutingConfig), String> {
    let models = merge_models_config(value.get("models"))?;
    let routing: RoutingConfig = match value.get("routing") {
        Some(r) if !r.is_null() => {
            serde_json::from_value(r.clone()).map_err(|e| format!("routing: {e}"))?
        }
        _ => RoutingConfig::default(),
    };
    validate_models_config(&models, &routing).map_err(|errs| errs.join("; "))?;
    Ok((models, routing))
}

/// Map a CONFIG-ERROR string to an `io::Error` with the `CONFIG ERROR:` prefix.
fn config_err(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("CONFIG ERROR: {message}"),
    )
}

/// Refuse a mutating verb on a config that still carries legacy model keys.
///
/// The `show`/`list` read verbs are deliberately NOT guarded (they detect and
/// instruct); only the mutating verbs hard-error so an operator can't keep
/// accreting legacy config the loop rejects. The sanctioned migration is
/// `models init --force-replace-legacy`. A missing/malformed config is not
/// legacy — the verb proceeds (the read/write paths handle malformed JSON).
fn reject_legacy_project_config(db_dir: &Path) -> io::Result<()> {
    let path = db_dir.join("config.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Ok(());
    };
    let legacy = detect_legacy_model_keys(&value);
    if legacy.is_empty() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        legacy_model_keys_message(&legacy),
    ))
}

/// Common tail for the simple setters: validate the mutated value, write it.
fn validate_and_write(db_dir: &Path, value: &serde_json::Value) -> io::Result<()> {
    build_and_validate(value).map_err(config_err)?;
    write_config_value(db_dir, value)
}

// ============================================================================
// init
// ============================================================================

/// `task-mgr models init` — write the FR-001 default `models`/`routing` block.
///
/// - Plain `init` on a clean config writes the default block (preserving unknown
///   keys). On a config that still carries legacy keys it HARD-ERRORS, pointing
///   at `--force-replace-legacy`.
/// - `--force-replace-legacy` deletes exactly the four legacy keys, then writes
///   the default block.
/// - `--dry-run` prints both halves of the diff (legacy keys + new block) and
///   writes nothing.
pub fn handle_init(db_dir: &Path, force_replace_legacy: bool, dry_run: bool) -> io::Result<()> {
    let mut value = read_config_value(db_dir)?;
    let legacy = detect_legacy_model_keys(&value);
    let block = fr_001_default_block();

    if dry_run {
        ui::emit_data("models init --dry-run (nothing will be written)");
        ui::emit_data("");
        if legacy.is_empty() {
            ui::emit_data("Legacy keys --force-replace-legacy would delete: (none)");
        } else {
            ui::emit_data("Legacy keys --force-replace-legacy would delete:");
            for key in &legacy {
                let cur = value
                    .get(*key)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string());
                ui::emit_data(&format!("  - {key}: {cur}"));
            }
        }
        ui::emit_data("");
        ui::emit_data("New models/routing block that would be written:");
        ui::emit_data(&serde_json::to_string_pretty(&block).unwrap_or_default());
        return Ok(());
    }

    if !legacy.is_empty() && !force_replace_legacy {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            legacy_model_keys_message(&legacy),
        ));
    }

    let obj = value.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "config.json is not a JSON object",
        )
    })?;
    if force_replace_legacy {
        for key in LEGACY_MODEL_KEYS {
            obj.remove(*key);
        }
    }
    obj.insert("models".to_string(), block["models"].clone());
    obj.insert("routing".to_string(), block["routing"].clone());

    // AC#10: a config written by init must validate clean.
    build_and_validate(&value).map_err(config_err)?;
    write_config_value(db_dir, &value)?;

    if force_replace_legacy && !legacy.is_empty() {
        ui::emit_data(&format!(
            "Removed legacy key(s) [{}] and wrote the FR-001 default models/routing block.",
            legacy.join(", ")
        ));
    } else {
        ui::emit_data("Wrote the FR-001 default models/routing block to .task-mgr/config.json.");
    }
    Ok(())
}

/// Write the FR-001 default `models`/`routing` block with `anchor` pinned, used
/// by the `task-mgr init` scaffold anchor picker (FR-009). Preserves unknown
/// keys; validates before writing. Callers must have already confirmed the
/// config carries no legacy keys and no existing `models` block.
pub fn write_default_block_with_anchor(db_dir: &Path, anchor: CapabilityTier) -> io::Result<()> {
    let mut value = read_config_value(db_dir)?;
    let block = fr_001_default_block();
    let obj = value.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "config.json is not a JSON object",
        )
    })?;
    obj.insert("models".to_string(), block["models"].clone());
    obj.insert("routing".to_string(), block["routing"].clone());
    set_json_path(
        &mut value,
        &["models", "anchor"],
        Some(serde_json::Value::String(anchor.as_str().to_string())),
    )?;
    build_and_validate(&value).map_err(config_err)?;
    write_config_value(db_dir, &value)
}

// ============================================================================
// set-anchor / enable / disable / set-tier / unset-tier / set-effort
// set-fallback / unset-fallback / route / unroute
// ============================================================================

/// `task-mgr models set-anchor <tier>`.
pub fn handle_set_anchor(db_dir: &Path, tier: &str) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let parsed =
        CapabilityTier::parse(tier).map_err(|e| config_err(format!("models.anchor: {e}")))?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "anchor"],
        Some(serde_json::Value::String(parsed.as_str().to_string())),
    )?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!("Set anchor tier to {}", parsed.as_str()));
    Ok(())
}

/// `task-mgr models enable|disable <provider>`. `enable` probes the provider
/// binary BEFORE writing; `disable` never probes.
pub fn handle_set_enabled(db_dir: &Path, provider: &str, enable: bool) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "providers", p.as_str(), "enabled"],
        Some(serde_json::Value::Bool(enable)),
    )?;
    let (models, routing) = build_and_validate(&value).map_err(config_err)?;

    if enable {
        // Probe BEFORE writing — enabling a provider introduces its binary as a
        // requirement; surface a missing binary now, not at loop startup.
        let resolved = resolve_models_config(&models, &routing);
        probe_enabled_provider_binaries(&resolved).map_err(|e| io::Error::other(e.to_string()))?;
    }

    write_config_value(db_dir, &value)?;
    ui::emit_data(&format!(
        "{} provider {}",
        if enable { "Enabled" } else { "Disabled" },
        p.as_str()
    ));
    Ok(())
}

/// `task-mgr models set-tier <provider> <tier> [model]`. Omitting `model` writes
/// `null` (route with no model flag — the codex shape).
pub fn handle_set_tier(
    db_dir: &Path,
    provider: &str,
    tier: &str,
    model: Option<&str>,
) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    let t = CapabilityTier::parse(tier).map_err(|e| config_err(format!("tier: {e}")))?;
    let leaf = match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => serde_json::Value::String(m.to_string()),
        None => serde_json::Value::Null,
    };
    let display = match &leaf {
        serde_json::Value::String(m) => m.clone(),
        _ => "(no model flag)".to_string(),
    };
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "providers", p.as_str(), "tiers", t.as_str()],
        Some(leaf),
    )?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!(
        "Set {}.tiers.{} -> {display}",
        p.as_str(),
        t.as_str()
    ));
    Ok(())
}

/// `task-mgr models unset-tier <provider> <tier>` — removes the operator's tier
/// override (the built-in default ladder for that rung, if any, reapplies).
pub fn handle_unset_tier(db_dir: &Path, provider: &str, tier: &str) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    let t = CapabilityTier::parse(tier).map_err(|e| config_err(format!("tier: {e}")))?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "providers", p.as_str(), "tiers", t.as_str()],
        None,
    )?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!(
        "Cleared {}.tiers.{} override (default rung, if any, reapplies)",
        p.as_str(),
        t.as_str()
    ));
    Ok(())
}

/// `task-mgr models set-effort <provider> <difficulty> [effort]`. Omitting
/// `effort` writes `null` (no effort flag for that difficulty).
pub fn handle_set_effort(
    db_dir: &Path,
    provider: &str,
    difficulty: &str,
    effort: Option<&str>,
) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    if difficulty_rank(Some(difficulty)).is_none() {
        return Err(config_err(format!(
            "difficulty {difficulty:?} is not one of: low, medium, high"
        )));
    }
    let key = difficulty.trim().to_ascii_lowercase();
    let leaf = match effort.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => serde_json::Value::String(e.to_string()),
        None => serde_json::Value::Null,
    };
    let display = match &leaf {
        serde_json::Value::String(e) => e.clone(),
        _ => "(no effort flag)".to_string(),
    };
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "providers", p.as_str(), "effort", &key],
        Some(leaf),
    )?;
    // build_and_validate enforces the codex `xhigh` policy cap and the grok
    // CLI constraint (high|medium|low only).
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!("Set {}.effort.{key} -> {display}", p.as_str()));
    Ok(())
}

/// `task-mgr models set-fallback <provider> <target>` — set the tier-preserving
/// cross-provider fallback target (new per-provider semantics, replacing the
/// legacy global `fallbackRunner` block).
pub fn handle_set_fallback(db_dir: &Path, provider: &str, target: &str) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    let tgt =
        parse_config_provider(target).map_err(|e| config_err(format!("fallback target: {e}")))?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "providers", p.as_str(), "fallback"],
        Some(serde_json::Value::String(tgt.as_str().to_string())),
    )?;
    // build_and_validate rejects self-fallback and a disabled / unknown target.
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!("Set {}.fallback -> {}", p.as_str(), tgt.as_str()));
    Ok(())
}

/// `task-mgr models unset-fallback <provider>`.
pub fn handle_unset_fallback(db_dir: &Path, provider: &str) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["models", "providers", p.as_str(), "fallback"],
        None,
    )?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!("Cleared {}.fallback", p.as_str()));
    Ok(())
}

/// `task-mgr models route <prefix> --provider <p> [--tier <t>]` — add/replace a
/// `routing.byIdPrefix` forced route.
pub fn handle_route(
    db_dir: &Path,
    prefix: &str,
    provider: &str,
    tier: Option<&str>,
) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    if prefix.trim().is_empty() {
        return Err(config_err("route prefix must not be blank"));
    }
    let p = parse_config_provider(provider).map_err(|e| config_err(format!("provider: {e}")))?;
    let mut route = serde_json::Map::new();
    route.insert(
        "provider".to_string(),
        serde_json::Value::String(p.as_str().to_string()),
    );
    if let Some(t) = tier {
        let parsed = CapabilityTier::parse(t).map_err(|e| config_err(format!("tier: {e}")))?;
        route.insert(
            "tier".to_string(),
            serde_json::Value::String(parsed.as_str().to_string()),
        );
    }
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["routing", "byIdPrefix", prefix],
        Some(serde_json::Value::Object(route)),
    )?;
    // build_and_validate rejects a route to a disabled / unknown provider.
    validate_and_write(db_dir, &value)?;
    let tier_note = tier
        .map(|t| format!(" [tier: {}]", t.trim().to_ascii_lowercase()))
        .unwrap_or_default();
    ui::emit_data(&format!(
        "Routed byIdPrefix[{prefix}] -> {}{tier_note}",
        p.as_str()
    ));
    Ok(())
}

/// `task-mgr models unroute <prefix>` — remove a `routing.byIdPrefix` route.
pub fn handle_unroute(db_dir: &Path, prefix: &str) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(&mut value, &["routing", "byIdPrefix", prefix], None)?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!("Removed byIdPrefix[{prefix}] route"));
    Ok(())
}

/// Accepted `--on-low` values (CLI wire form; matches serde camelCase of
/// [`OnLowAction`]).
const ON_LOW_ACCEPTED: &str = "wait, unavailable, stop, ask, ignore";

fn parse_on_low(raw: &str) -> Result<OnLowAction, io::Error> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "wait" => Ok(OnLowAction::Wait),
        "unavailable" => Ok(OnLowAction::Unavailable),
        "stop" => Ok(OnLowAction::Stop),
        "ask" => Ok(OnLowAction::Ask),
        "ignore" => Ok(OnLowAction::Ignore),
        other => Err(config_err(format!(
            "on-low {other:?} is not one of: {ON_LOW_ACCEPTED}"
        ))),
    }
}

fn on_low_wire(action: OnLowAction) -> &'static str {
    match action {
        OnLowAction::Wait => "wait",
        OnLowAction::Unavailable => "unavailable",
        OnLowAction::Stop => "stop",
        OnLowAction::Ask => "ask",
        OnLowAction::Ignore => "ignore",
    }
}

/// `task-mgr models set-usage-rule --kind/--id --on-low` — append or replace a
/// `usagePolicy.rules` entry. At least one of `--kind` / `--id` is required.
pub fn handle_set_usage_rule(
    db_dir: &Path,
    kind: Option<&str>,
    id: Option<&str>,
    on_low: &str,
) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let kind = kind.map(str::trim).filter(|s| !s.is_empty());
    let id = id.map(str::trim).filter(|s| !s.is_empty());
    if kind.is_none() && id.is_none() {
        return Err(config_err(
            "set-usage-rule requires --kind and/or --id (at least one must be set)",
        ));
    }
    let action = parse_on_low(on_low)?;

    let mut rule = serde_json::Map::new();
    if let Some(k) = kind {
        rule.insert(
            "kind".to_string(),
            serde_json::Value::String(k.to_string()),
        );
    }
    if let Some(i) = id {
        rule.insert("id".to_string(), serde_json::Value::String(i.to_string()));
    }
    rule.insert(
        "onLow".to_string(),
        serde_json::Value::String(on_low_wire(action).to_string()),
    );
    let rule_val = serde_json::Value::Object(rule);

    let mut value = read_config_value(db_dir)?;
    // Ensure usagePolicy.rules is an array, then append-or-replace by kind+id identity.
    let usage = value
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "config.json is not a JSON object",
            )
        })?
        .entry("usagePolicy".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !usage.is_object() {
        *usage = serde_json::json!({});
    }
    let rules = usage
        .as_object_mut()
        .expect("usagePolicy object")
        .entry("rules".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !rules.is_array() {
        *rules = serde_json::json!([]);
    }
    let arr = rules.as_array_mut().expect("rules array");
    let replace_at = arr.iter().position(|existing| {
        let ek = existing.get("kind").and_then(|v| v.as_str());
        let ei = existing.get("id").and_then(|v| v.as_str());
        ek == kind && ei == id
    });
    match replace_at {
        Some(i) => arr[i] = rule_val,
        None => arr.push(rule_val),
    }

    validate_and_write(db_dir, &value)?;
    let mut parts = Vec::new();
    if let Some(k) = kind {
        parts.push(format!("kind={k}"));
    }
    if let Some(i) = id {
        parts.push(format!("id={i}"));
    }
    ui::emit_data(&format!(
        "Set usagePolicy.rules[{}] onLow={}",
        parts.join(", "),
        on_low_wire(action)
    ));
    Ok(())
}

/// `task-mgr models set-tier-fallback <low|medium|high> [--include-review]
/// [--include-forced]`. Omitted flags keep struct defaults
/// (`includeReview: true`, `includeForced: false`).
pub fn handle_set_tier_fallback(
    db_dir: &Path,
    difficulty: &str,
    include_review: bool,
    include_forced: bool,
) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    if difficulty_rank(Some(difficulty)).is_none() {
        return Err(config_err(format!(
            "difficulty {difficulty:?} is not one of: low, medium, high"
        )));
    }
    let key = difficulty.trim().to_ascii_lowercase();
    let leaf = serde_json::json!({
        "maxDifficulty": key,
        "includeReview": include_review,
        "includeForced": include_forced,
    });
    let mut value = read_config_value(db_dir)?;
    set_json_path(&mut value, &["routing", "tierFallback"], Some(leaf))?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data(&format!(
        "Set routing.tierFallback maxDifficulty={key} includeReview={include_review} \
         includeForced={include_forced}"
    ));
    Ok(())
}

/// `task-mgr models unset-tier-fallback` — write JSON `null` (ask opt-out).
///
/// Must NOT delete the key: an absent `tierFallback` deserializes to factory
/// `Some`, which is the opposite of the ask opt-out.
pub fn handle_unset_tier_fallback(db_dir: &Path) -> io::Result<()> {
    reject_legacy_project_config(db_dir)?;
    let mut value = read_config_value(db_dir)?;
    set_json_path(
        &mut value,
        &["routing", "tierFallback"],
        Some(serde_json::Value::Null),
    )?;
    validate_and_write(db_dir, &value)?;
    ui::emit_data("Set routing.tierFallback to null (ask opt-out)");
    Ok(())
}

// ============================================================================
// list
// ============================================================================

/// `task-mgr models list` entry point.
pub fn handle_list(db_dir: &Path, opts: ListOpts) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    handle_list_to(&mut out, db_dir, opts)
}

/// Testable variant that writes to an arbitrary `Write`.
pub fn handle_list_to<W: io::Write>(
    writer: &mut W,
    db_dir: &Path,
    opts: ListOpts,
) -> io::Result<()> {
    let want_remote = opts.remote || opts.refresh;
    if opts.refresh {
        cache::invalidate();
    }

    if want_remote {
        match fetch_live_list() {
            Ok(remote) => {
                writeln!(writer, "Models (live from Anthropic /v1/models):")?;
                writeln!(writer)?;
                for m in &remote {
                    let date = m
                        .created_at
                        .map(|dt| dt.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "—".to_string());
                    let name = m.display_name.as_deref().unwrap_or("");
                    writeln!(writer, "  {}  {:<30}  ({})", date, m.id, name)?;
                }
                return Ok(());
            }
            Err(ApiError::NoKey) | Err(ApiError::NotOptedIn) => {
                // Silent fallback per the design.
            }
            Err(e) => {
                ui::emit_err(&format!(
                    "\x1b[33m[warn]\x1b[0m live model fetch failed: {e}; using offline list"
                ));
            }
        }
    }

    let cfg = read_project_config(db_dir);
    writeln!(
        writer,
        "Configured provider ladders (merged config + built-in defaults):"
    )?;
    writeln!(writer)?;
    render_provider_ladders(writer, &cfg.models)?;
    if !want_remote {
        writeln!(writer)?;
        writeln!(
            writer,
            "(run with --remote to fetch the live Anthropic catalog; requires \
             ANTHROPIC_API_KEY + TASK_MGR_USE_API=1)"
        )?;
    }
    Ok(())
}

// ============================================================================
// show
// ============================================================================

/// `task-mgr models show` entry point.
pub fn handle_show(db_dir: &Path, db_dir_source: crate::db::DbDirSource) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    handle_show_to(&mut out, db_dir, db_dir_source)
}

/// Testable variant of `handle_show` that writes to an arbitrary `Write`.
pub fn handle_show_to<W: io::Write>(
    writer: &mut W,
    db_dir: &Path,
    db_dir_source: crate::db::DbDirSource,
) -> io::Result<()> {
    let cfg = read_project_config(db_dir);
    let resolved = resolve_models_config(&cfg.models, &cfg.routing);
    // Raw Value needed to distinguish omitted tierFallback (factory) from
    // explicit JSON null (ask opt-out) — typed RoutingConfig collapses both
    // into Option and applies the serde default for absent keys.
    let raw = read_config_value(db_dir).unwrap_or_else(|_| serde_json::json!({ "version": 1 }));

    // Legacy detect-and-instruct banner (read verbs NEVER hard-fail on legacy).
    if let Some(legacy) = detect_legacy_keys_in_file(db_dir) {
        writeln!(
            writer,
            "⚠ legacy model key(s) present and IGNORED: [{}]. Migrate with \
             `task-mgr models init --force-replace-legacy`.",
            legacy.join(", ")
        )?;
        writeln!(writer)?;
    }

    writeln!(writer, "models:")?;
    writeln!(
        writer,
        "  primaryProvider: {}",
        resolved.primary_provider.as_str()
    )?;
    writeln!(writer, "  anchor:          {}", resolved.anchor.as_str())?;
    writeln!(writer)?;
    render_provider_ladders(writer, &cfg.models)?;

    writeln!(writer)?;
    render_routing(writer, &cfg.routing, raw.get("routing"))?;

    writeln!(writer)?;
    render_usage_policy(writer, &cfg.usage_policy)?;

    // Remaining numbers ONLY behind the same live-fetch gate as `models list
    // --remote` (`TASK_MGR_USE_API=1` via check_opt_in). Offline show must not
    // print `% left`. Silent fallback when not opted in (not a hard error).
    if check_opt_in().is_ok()
        && let Some(info) = load_usage_info()
    {
        let floor = cfg.usage_policy.remaining_min_percent;
        if let Some(banner) = remaining_banner_for_run_models(&info, &resolved, floor, Utc::now()) {
            writeln!(writer)?;
            writeln!(writer, "remaining (live): {banner}")?;
        }
    }

    writeln!(writer)?;
    render_anchor_mapping(writer, &resolved)?;

    writeln!(writer)?;
    render_crash_escalation(writer, &resolved)?;

    writeln!(writer)?;
    writeln!(
        writer,
        "Note: Codex pinning is route-only — `task-mgr models route <PREFIX> \
         --provider codex`; a per-task `tasks.model` value cannot express Codex \
         (provider inference never yields Codex)."
    )?;

    writeln!(writer)?;
    writeln!(
        writer,
        "db_dir: {}  (source: {})",
        db_dir.display(),
        db_dir_source.label()
    )
}

// ============================================================================
// Rendering helpers
// ============================================================================

/// Human-readable display name for a [`Provider`].
fn provider_label(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Claude",
        Provider::Grok => "Grok",
        Provider::Codex => "Codex",
    }
}

/// Render each provider's resolved tier ladder + effort table + fallback, in
/// `DISPLAY_PROVIDERS` order, tiers in `CapabilityTier::ALL` order.
fn render_provider_ladders<W: io::Write>(writer: &mut W, models: &ModelsConfig) -> io::Result<()> {
    for provider in DISPLAY_PROVIDERS {
        let Some(pc) = models.providers.get(provider.as_str()) else {
            continue;
        };
        render_one_provider(writer, provider, pc)?;
    }
    Ok(())
}

fn render_one_provider<W: io::Write>(
    writer: &mut W,
    provider: Provider,
    pc: &ProviderConfig,
) -> io::Result<()> {
    let state = if pc.enabled { "enabled" } else { "disabled" };
    writeln!(writer, "  {} ({state})", provider_label(provider))?;
    writeln!(writer, "    tiers:")?;
    let mut any_tier = false;
    for tier in CapabilityTier::ALL {
        match pc.tiers.get(tier.as_str()) {
            None => continue, // undefined rung
            Some(model) => {
                any_tier = true;
                let value = model.as_deref().unwrap_or("(no model flag)");
                writeln!(writer, "      {:<14} -> {value}", tier.as_str())?;
            }
        }
    }
    if !any_tier {
        writeln!(writer, "      (no defined rungs)")?;
    }
    let mut any_effort = false;
    let mut effort_line = String::from("    effort:");
    for d in DIFFICULTIES {
        if let Some(level) = pc.effort.get(d) {
            any_effort = true;
            let v = level.as_deref().unwrap_or("(none)");
            effort_line.push_str(&format!(" {d}->{v}"));
        }
    }
    if any_effort {
        writeln!(writer, "{effort_line}")?;
    } else {
        writeln!(writer, "    effort: (none)")?;
    }
    match &pc.fallback {
        Some(fb) => writeln!(writer, "    fallback: {fb}")?,
        None => writeln!(writer, "    fallback: (none)")?,
    }
    Ok(())
}

/// Render the `routing` block with explicit empty states.
///
/// `raw_routing` is the raw JSON `routing` object (if any) so `tierFallback`
/// can distinguish omitted (factory) from explicit `null` (ask opt-out).
fn render_routing<W: io::Write>(
    writer: &mut W,
    routing: &RoutingConfig,
    raw_routing: Option<&serde_json::Value>,
) -> io::Result<()> {
    writeln!(writer, "routing:")?;

    writeln!(writer, "  byIdPrefix:")?;
    if routing.by_id_prefix.is_empty() {
        writeln!(writer, "    (no routes)")?;
    } else {
        let mut routes: Vec<_> = routing.by_id_prefix.iter().collect();
        routes.sort_by_key(|(k, _)| k.as_str());
        for (prefix, spec) in routes {
            let pname = parse_config_provider(&spec.provider)
                .map(provider_label)
                .unwrap_or(spec.provider.as_str());
            let tier_note = spec
                .tier
                .as_deref()
                .map(|t| format!(" [tier: {t}]"))
                .unwrap_or_default();
            writeln!(writer, "    {prefix} -> {pname}{tier_note}")?;
        }
    }

    writeln!(writer, "  taskClasses:")?;
    if routing.task_classes.is_empty() {
        writeln!(writer, "    (no routes)")?;
    } else {
        let mut classes: Vec<_> = routing.task_classes.iter().collect();
        classes.sort_by_key(|(k, _)| k.as_str());
        for (class, route) in classes {
            let prefs = if route.provider_preference.is_empty() {
                "(default)".to_string()
            } else {
                route.provider_preference.join(", ")
            };
            let tier_note = route
                .force_tier
                .as_deref()
                .map(|t| format!(", forceTier: {t}"))
                .unwrap_or_default();
            writeln!(writer, "    {class} -> providers: [{prefs}]{tier_note}")?;
        }
    }

    match &routing.spillover.max_difficulty {
        Some(d) => writeln!(writer, "  spillover: maxDifficulty={d}")?,
        None => writeln!(writer, "  spillover: disabled")?,
    }

    render_tier_fallback(writer, raw_routing)?;
    Ok(())
}

/// Print `routing.tierFallback` from the raw JSON so null ≠ omitted.
fn render_tier_fallback<W: io::Write>(
    writer: &mut W,
    raw_routing: Option<&serde_json::Value>,
) -> io::Result<()> {
    let Some(routing_obj) = raw_routing.and_then(|v| v.as_object()) else {
        // No routing key at all → same as omitted tierFallback → factory.
        writeln!(
            writer,
            "  tierFallback: (factory default: maxDifficulty=high includeReview=true \
             includeForced=false)"
        )?;
        return Ok(());
    };
    match routing_obj.get("tierFallback") {
        None => writeln!(
            writer,
            "  tierFallback: (factory default: maxDifficulty=high includeReview=true \
             includeForced=false)"
        )?,
        Some(v) if v.is_null() => writeln!(writer, "  tierFallback: (unset)")?,
        Some(v) => {
            let max = v
                .get("maxDifficulty")
                .and_then(|x| x.as_str())
                .unwrap_or("high");
            let include_review = v
                .get("includeReview")
                .and_then(|x| x.as_bool())
                .unwrap_or(true);
            let include_forced = v
                .get("includeForced")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            writeln!(
                writer,
                "  tierFallback: maxDifficulty={max} includeReview={include_review} \
                 includeForced={include_forced}"
            )?;
        }
    }
    Ok(())
}

/// Print `usagePolicy` knobs + rules (always; remaining numbers are separate).
fn render_usage_policy<W: io::Write>(writer: &mut W, policy: &UsagePolicy) -> io::Result<()> {
    writeln!(writer, "usagePolicy:")?;
    writeln!(
        writer,
        "  remainingMinPercent: {}",
        policy.remaining_min_percent
    )?;
    writeln!(
        writer,
        "  waitIfResetWithinMinutes: {}",
        policy.wait_if_reset_within_minutes
    )?;
    writeln!(
        writer,
        "  stopIfResetBeyondHours: {}",
        policy.stop_if_reset_beyond_hours
    )?;
    writeln!(writer, "  askTtlMinutes: {}", policy.ask_ttl_minutes)?;
    writeln!(writer, "  rules:")?;
    if policy.rules.is_empty() {
        writeln!(writer, "    (none)")?;
    } else {
        for (i, rule) in policy.rules.iter().enumerate() {
            let mut parts = Vec::new();
            if let Some(k) = rule.kind.as_deref() {
                parts.push(format!("kind={k}"));
            }
            if let Some(id) = rule.id.as_deref() {
                parts.push(format!("id={id}"));
            }
            parts.push(format!("onLow={}", on_low_wire(rule.on_low)));
            writeln!(writer, "    [{i}] {}", parts.join(" "))?;
        }
    }
    Ok(())
}

/// Render the anchor-derived difficulty→model mapping for the primary provider.
fn render_anchor_mapping<W: io::Write>(
    writer: &mut W,
    resolved: &ResolvedModelsConfig,
) -> io::Result<()> {
    let primary = resolved.primary_provider;
    writeln!(
        writer,
        "Anchor-derived difficulty -> model (primaryProvider: {}, anchor: {}):",
        primary.as_str(),
        resolved.anchor.as_str()
    )?;
    for d in DIFFICULTIES {
        let tier = anchored_tier(resolved.anchor, Some(d));
        let model = resolved
            .model_for(primary, tier)
            .unwrap_or("(provider default — no model flag)");
        writeln!(writer, "  {:<7} -> {model}  [{}]", d, tier.as_str())?;
    }
    Ok(())
}

/// Render the one-line crash-escalation cost note: anchor+1, the resolved model
/// it lands on, and the relative cost.
fn render_crash_escalation<W: io::Write>(
    writer: &mut W,
    resolved: &ResolvedModelsConfig,
) -> io::Result<()> {
    let primary = resolved.primary_provider;
    let anchor_model = resolved.model_for(primary, resolved.anchor);
    let escalated = escalate_tier(resolved, primary, anchor_model);
    match (anchor_model, escalated.as_deref()) {
        (Some(base), Some(up)) if up != base => writeln!(
            writer,
            "Crash escalation: anchor+1 -> {up} (one capability tier above the \
             anchor's {base}; higher tier/cost)"
        ),
        (Some(base), _) => writeln!(
            writer,
            "Crash escalation: {base} is already at the top defined tier for {} \
             — no higher rung to escalate to",
            primary.as_str()
        ),
        _ => writeln!(
            writer,
            "Crash escalation: primaryProvider {} has no model at the anchor tier \
             (routes with no model flag)",
            primary.as_str()
        ),
    }
}

// ============================================================================
// Shared read helpers (used by show + the legacy banner)
// ============================================================================

/// Detect legacy model keys present in the raw config file, or `None` when the
/// file is absent / malformed / clean. Used for the `show` banner only — read
/// verbs never hard-fail on legacy keys.
fn detect_legacy_keys_in_file(db_dir: &Path) -> Option<Vec<&'static str>> {
    let contents = std::fs::read_to_string(db_dir.join("config.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let legacy = detect_legacy_model_keys(&value);
    if legacy.is_empty() {
        None
    } else {
        Some(legacy)
    }
}

fn fetch_live_list() -> Result<Vec<super::api::RemoteModel>, ApiError> {
    check_opt_in()?;
    if let Some(cached) = cache::read_fresh() {
        return Ok(cached);
    }
    let mut fresh = fetch_models()?;
    sort_newest_first(&mut fresh);
    cache::write(&fresh);
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbDirSource;
    use crate::loop_engine::model::{FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
    use crate::loop_engine::project_config::validate_models_config;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;

    fn read_raw(db_dir: &Path) -> String {
        fs::read_to_string(db_dir.join("config.json")).unwrap()
    }

    fn read_value(db_dir: &Path) -> serde_json::Value {
        serde_json::from_str(&read_raw(db_dir)).unwrap()
    }

    /// Read the merged config and assert it validates clean (the AC#10 invariant).
    fn assert_validates_clean(db_dir: &Path) {
        let cfg = read_project_config(db_dir);
        validate_models_config(&cfg.models, &cfg.routing)
            .expect("config must validate clean through validate_models_config");
    }

    fn show_output(db_dir: &Path) -> String {
        let mut buf = Cursor::new(Vec::new());
        handle_show_to(&mut buf, db_dir, DbDirSource::CwdDefault).unwrap();
        String::from_utf8(buf.into_inner()).unwrap()
    }

    fn list_output(db_dir: &Path) -> String {
        let mut buf = Cursor::new(Vec::new());
        handle_list_to(&mut buf, db_dir, ListOpts::default()).unwrap();
        String::from_utf8(buf.into_inner()).unwrap()
    }

    /// Create an executable file (Unix mode 0755) and return its path string.
    #[cfg(unix)]
    fn make_executable(dir: &Path, name: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path.to_str().unwrap().to_string()
    }

    // ---- init -------------------------------------------------------------

    #[test]
    fn init_writes_default_block_and_validates_clean() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"embeddingModel":"x"}"#,
        )
        .unwrap();
        handle_init(dir.path(), false, false).unwrap();

        let v = read_value(dir.path());
        assert_eq!(v["models"]["anchor"], "standard");
        assert_eq!(v["models"]["primaryProvider"], "claude");
        assert_eq!(v["models"]["providers"]["claude"]["enabled"], true);
        assert!(v.get("routing").is_some(), "routing block must be written");
        // Unknown keys preserved.
        assert!(read_raw(dir.path()).contains("additionalAllowedTools"));
        assert!(read_raw(dir.path()).contains("embeddingModel"));
        assert_validates_clean(dir.path());
    }

    #[test]
    fn init_on_legacy_config_without_force_hard_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"defaultModel":"x","reviewModel":"y"}"#,
        )
        .unwrap();
        let err = handle_init(dir.path(), false, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("defaultModel"), "must name legacy key: {msg}");
        assert!(msg.contains("reviewModel"), "must name legacy key: {msg}");
        assert!(
            msg.contains("models init --force-replace-legacy"),
            "must point at the migration: {msg}"
        );
    }

    #[test]
    fn init_force_replace_legacy_deletes_exactly_four_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"defaultModel":"a","reviewModel":"b","primaryRunner":{"byIdPrefix":{}},
                "fallbackRunner":{"enabled":true},"additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();
        handle_init(dir.path(), true, false).unwrap();

        let raw = read_raw(dir.path());
        for key in [
            "defaultModel",
            "reviewModel",
            "primaryRunner",
            "fallbackRunner",
        ] {
            assert!(
                !raw.contains(key),
                "legacy key {key} must be deleted:\n{raw}"
            );
        }
        assert!(
            raw.contains("additionalAllowedTools"),
            "unknown key must survive migration:\n{raw}"
        );
        let v = read_value(dir.path());
        assert_eq!(v["models"]["anchor"], "standard");
        assert_validates_clean(dir.path());
    }

    #[test]
    fn init_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"{"version":1,"defaultModel":"a"}"#;
        fs::write(dir.path().join("config.json"), original).unwrap();
        handle_init(dir.path(), true, true).unwrap();
        // Dry-run never writes — even with --force-replace-legacy.
        assert_eq!(read_raw(dir.path()), original);
    }

    // ---- set-anchor -------------------------------------------------------

    #[test]
    fn set_anchor_writes_normalized_tier_and_preserves_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"embeddingModel":"x"}"#,
        )
        .unwrap();
        handle_set_anchor(dir.path(), "  COST-EFFICIENT ").unwrap();
        let v = read_value(dir.path());
        assert_eq!(v["models"]["anchor"], "cost-efficient");
        assert!(read_raw(dir.path()).contains("embeddingModel"));
        assert_validates_clean(dir.path());
    }

    #[test]
    fn set_anchor_rejects_typo_as_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_set_anchor(dir.path(), "fronteir").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("CONFIG ERROR"),
            "must be a CONFIG ERROR: {msg}"
        );
        assert!(
            msg.contains("cheapest") && msg.contains("frontier"),
            "must name the accepted set: {msg}"
        );
        assert!(
            !dir.path().join("config.json").exists(),
            "no file written on validation failure"
        );
    }

    // ---- enable / disable -------------------------------------------------

    #[test]
    fn disable_provider_never_probes_and_writes_false() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_enabled(dir.path(), "grok", false).unwrap();
        let v = read_value(dir.path());
        assert_eq!(v["models"]["providers"]["grok"]["enabled"], false);
        assert_validates_clean(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn enable_provider_probes_before_write_success() {
        use crate::loop_engine::test_utils::{CLAUDE_BINARY_MUTEX, EnvGuard, GROK_BINARY_MUTEX};
        let _c = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _g = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let exe = make_executable(dir.path(), "fake-bin");
        let _eg = EnvGuard::set("GROK_BINARY", &exe);
        let _ec = EnvGuard::set("CLAUDE_BINARY", &exe);

        handle_set_enabled(dir.path(), "grok", true).unwrap();
        let v = read_value(dir.path());
        assert_eq!(v["models"]["providers"]["grok"]["enabled"], true);
    }

    #[cfg(unix)]
    #[test]
    fn enable_provider_missing_binary_leaves_config_unchanged() {
        use crate::loop_engine::test_utils::{CLAUDE_BINARY_MUTEX, EnvGuard, GROK_BINARY_MUTEX};
        let _c = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _g = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let exe = make_executable(dir.path(), "fake-bin");
        // Claude resolves (so the probe reaches grok); grok points at nothing.
        let _ec = EnvGuard::set("CLAUDE_BINARY", &exe);
        let _eg = EnvGuard::set("GROK_BINARY", "/tmp/task-mgr-no-such-grok-binary-feat009");

        let original = r#"{"version":1}"#;
        fs::write(dir.path().join("config.json"), original).unwrap();
        let err = handle_set_enabled(dir.path(), "grok", true).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("grok"), "{err}");
        assert_eq!(
            read_raw(dir.path()),
            original,
            "config must be unchanged on probe failure"
        );
    }

    // ---- set-tier / unset-tier -------------------------------------------

    #[test]
    fn set_tier_writes_model_and_preserves_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();
        // Pin OPUS to its own (standard) tier — re-stating a default rung is
        // unambiguous; pinning it to a *different* tier would clash with the
        // default standard→opus mapping (covered by the ambiguity test).
        handle_set_tier(dir.path(), "claude", "standard", Some(OPUS_MODEL)).unwrap();
        let v = read_value(dir.path());
        assert_eq!(
            v["models"]["providers"]["claude"]["tiers"]["standard"],
            OPUS_MODEL
        );
        assert!(read_raw(dir.path()).contains("additionalAllowedTools"));
        assert_validates_clean(dir.path());
    }

    #[test]
    fn set_tier_no_model_writes_null() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_tier(dir.path(), "codex", "standard", None).unwrap();
        let v = read_value(dir.path());
        assert!(
            v["models"]["providers"]["codex"]["tiers"]["standard"].is_null(),
            "omitted model must persist as JSON null"
        );
    }

    #[test]
    fn set_tier_ambiguous_reverse_lookup_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        // SONNET_MODEL is already the cost-efficient rung by default; also pinning
        // it to standard makes the reverse lookup ambiguous.
        let err =
            handle_set_tier(dir.path(), "claude", "standard", Some(SONNET_MODEL)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("ambiguous"), "must name the ambiguity: {msg}");
    }

    #[test]
    fn unset_tier_removes_override() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_tier(dir.path(), "claude", "standard", Some(OPUS_MODEL)).unwrap();
        handle_unset_tier(dir.path(), "claude", "standard").unwrap();
        let v = read_value(dir.path());
        assert!(
            v["models"]["providers"]["claude"]["tiers"]
                .get("standard")
                .is_none(),
            "standard override should be removed"
        );
    }

    // ---- set-effort -------------------------------------------------------

    #[test]
    fn set_effort_writes_level() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_effort(dir.path(), "claude", "high", Some("high")).unwrap();
        let v = read_value(dir.path());
        assert_eq!(v["models"]["providers"]["claude"]["effort"]["high"], "high");
        assert_validates_clean(dir.path());
    }

    #[test]
    fn set_effort_rejects_unknown_difficulty() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_set_effort(dir.path(), "claude", "trivial", Some("low")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(
            msg.contains("low, medium, high"),
            "must name accepted set: {msg}"
        );
    }

    #[test]
    fn set_effort_codex_xhigh_rejected_by_policy() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_set_effort(dir.path(), "codex", "high", Some("xhigh")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(
            msg.contains("policy"),
            "codex xhigh must be rejected by policy: {msg}"
        );
    }

    #[test]
    fn set_effort_grok_xhigh_rejected_by_cli_constraint() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-enable grok so the write path reaches effort validation.
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"models":{"providers":{"grok":{"enabled":true}}}}"#,
        )
        .unwrap();
        let err = handle_set_effort(dir.path(), "grok", "high", Some("xhigh")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(
            msg.contains("CLI") || msg.contains("grok"),
            "grok xhigh must be rejected as a CLI constraint: {msg}"
        );
    }

    // ---- set-fallback / unset-fallback -----------------------------------

    #[test]
    fn set_fallback_to_enabled_target_writes() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-enable grok so the fallback target is valid (avoids the probe).
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"models":{"providers":{"grok":{"enabled":true}}}}"#,
        )
        .unwrap();
        handle_set_fallback(dir.path(), "claude", "grok").unwrap();
        let v = read_value(dir.path());
        assert_eq!(v["models"]["providers"]["claude"]["fallback"], "grok");
        assert_validates_clean(dir.path());
    }

    #[test]
    fn set_fallback_to_self_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_set_fallback(dir.path(), "claude", "claude").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("itself"), "self-fallback must be named: {msg}");
    }

    #[test]
    fn set_fallback_to_disabled_target_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        // grok disabled by default → fallback target not enabled.
        let err = handle_set_fallback(dir.path(), "claude", "grok").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("not an enabled provider"), "{msg}");
    }

    #[test]
    fn unset_fallback_removes_override() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"models":{"providers":{"grok":{"enabled":true},"claude":{"fallback":"grok"}}}}"#,
        )
        .unwrap();
        handle_unset_fallback(dir.path(), "claude").unwrap();
        let v = read_value(dir.path());
        assert!(
            v["models"]["providers"]["claude"].get("fallback").is_none(),
            "fallback override should be removed"
        );
    }

    // ---- route / unroute --------------------------------------------------

    #[test]
    fn route_writes_byidprefix_with_tier() {
        let dir = tempfile::tempdir().unwrap();
        handle_route(dir.path(), "REVIEW-", "claude", Some("frontier")).unwrap();
        let v = read_value(dir.path());
        assert_eq!(v["routing"]["byIdPrefix"]["REVIEW-"]["provider"], "claude");
        assert_eq!(v["routing"]["byIdPrefix"]["REVIEW-"]["tier"], "frontier");
        assert_validates_clean(dir.path());
    }

    #[test]
    fn route_to_disabled_provider_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_route(dir.path(), "FEAT-", "codex", None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("not enabled"), "{msg}");
    }

    #[test]
    fn route_rejects_invalid_tier() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_route(dir.path(), "REVIEW-", "claude", Some("ultra")).unwrap_err();
        assert!(err.to_string().contains("CONFIG ERROR"), "{err}");
    }

    #[test]
    fn unroute_removes_route() {
        let dir = tempfile::tempdir().unwrap();
        handle_route(dir.path(), "REVIEW-", "claude", None).unwrap();
        handle_unroute(dir.path(), "REVIEW-").unwrap();
        let v = read_value(dir.path());
        assert!(
            v["routing"]["byIdPrefix"].get("REVIEW-").is_none(),
            "route should be removed"
        );
    }

    // ---- legacy guard on mutating verbs ----------------------------------

    #[test]
    fn mutating_verb_hard_errors_on_legacy_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"defaultModel":"x","fallbackRunner":{"enabled":true}}"#,
        )
        .unwrap();
        let err = handle_set_anchor(dir.path(), "standard").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("defaultModel"), "{msg}");
        assert!(msg.contains("fallbackRunner"), "{msg}");
        assert!(msg.contains("models init --force-replace-legacy"), "{msg}");
    }

    // ---- show -------------------------------------------------------------

    /// Build the exact anchor-mapping row prefix the renderer emits, so the
    /// assertion can't drift on whitespace.
    fn mapping_row(difficulty: &str, model: &str) -> String {
        format!("  {:<7} -> {model}", difficulty)
    }

    #[test]
    fn show_default_anchor_standard_maps_difficulties_to_models() {
        let dir = tempfile::tempdir().unwrap();
        let out = show_output(dir.path());
        assert!(
            out.contains(&mapping_row("low", SONNET_MODEL)),
            "low→sonnet at anchor standard:\n{out}"
        );
        assert!(
            out.contains(&mapping_row("medium", OPUS_MODEL)),
            "medium→opus at anchor standard:\n{out}"
        );
        assert!(
            out.contains(&mapping_row("high", FABLE_MODEL)),
            "high→fable at anchor standard:\n{out}"
        );
    }

    #[test]
    fn show_after_set_anchor_cost_efficient_shifts_window() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_anchor(dir.path(), "cost-efficient").unwrap();
        let out = show_output(dir.path());
        assert!(out.contains(&mapping_row("low", HAIKU_MODEL)), "{out}");
        assert!(out.contains(&mapping_row("medium", SONNET_MODEL)), "{out}");
        assert!(out.contains(&mapping_row("high", OPUS_MODEL)), "{out}");
    }

    #[test]
    fn show_renders_crash_escalation_and_codex_note() {
        let dir = tempfile::tempdir().unwrap();
        let out = show_output(dir.path());
        // Anchor standard → opus; crash escalation anchor+1 → fable.
        assert!(
            out.contains("Crash escalation"),
            "crash note missing:\n{out}"
        );
        assert!(
            out.contains(FABLE_MODEL),
            "crash escalation must resolve the +1 model:\n{out}"
        );
        assert!(
            out.contains("Codex pinning is route-only"),
            "codex route-only note missing:\n{out}"
        );
    }

    #[test]
    fn show_renders_explicit_empty_states() {
        let dir = tempfile::tempdir().unwrap();
        let out = show_output(dir.path());
        assert!(out.contains("byIdPrefix:"), "{out}");
        assert!(out.contains("(no routes)"), "empty routes state:\n{out}");
        assert!(out.contains("spillover: disabled"), "{out}");
        assert!(
            out.contains("disabled"),
            "grok/codex disabled state:\n{out}"
        );
    }

    #[test]
    fn show_emits_legacy_banner_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"defaultModel":"x"}"#,
        )
        .unwrap();
        let out = show_output(dir.path());
        assert!(
            out.contains("legacy model key(s) present"),
            "legacy banner missing:\n{out}"
        );
        assert!(
            out.contains("defaultModel"),
            "banner must name the key:\n{out}"
        );
    }

    #[test]
    fn show_round_trips_after_init() {
        let dir = tempfile::tempdir().unwrap();
        handle_init(dir.path(), false, false).unwrap();
        // AC#10: the init'd config validates clean, and show renders over it.
        assert_validates_clean(dir.path());
        let _ = show_output(dir.path());
    }

    // ---- list -------------------------------------------------------------

    #[test]
    fn list_offline_renders_provider_ladders() {
        let dir = tempfile::tempdir().unwrap();
        let out = list_output(dir.path());
        assert!(out.contains("Claude"), "{out}");
        assert!(
            out.contains(OPUS_MODEL),
            "claude ladder must list models:\n{out}"
        );
        assert!(out.contains(SONNET_MODEL), "{out}");
        assert!(out.contains(HAIKU_MODEL), "{out}");
        assert!(out.contains(FABLE_MODEL), "{out}");
        assert!(out.contains("Grok"), "{out}");
    }

    // ---- set-usage-rule / set-tier-fallback / show policy (FEAT-008) -------

    #[test]
    fn set_usage_rule_writes_camelcase_on_low_and_preserves_sparse_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"embeddingModel":"nomic"}"#,
        )
        .unwrap();
        handle_set_usage_rule(dir.path(), Some("weekly_scoped"), None, "unavailable").unwrap();
        let raw = read_raw(dir.path());
        assert!(
            raw.contains("\"onLow\""),
            "must write camelCase onLow:\n{raw}"
        );
        assert!(
            raw.contains("\"unavailable\""),
            "onLow value missing:\n{raw}"
        );
        assert!(
            raw.contains("additionalAllowedTools"),
            "sparse preserve tools:\n{raw}"
        );
        assert!(
            raw.contains("embeddingModel"),
            "sparse preserve embedding:\n{raw}"
        );
        let v = read_value(dir.path());
        assert_eq!(v["usagePolicy"]["rules"][0]["kind"], "weekly_scoped");
        assert_eq!(v["usagePolicy"]["rules"][0]["onLow"], "unavailable");
    }

    #[test]
    fn set_usage_rule_replaces_same_identity_and_rejects_bad_inputs() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_usage_rule(dir.path(), Some("weekly_scoped"), None, "wait").unwrap();
        handle_set_usage_rule(dir.path(), Some("weekly_scoped"), None, "ask").unwrap();
        let v = read_value(dir.path());
        let rules = v["usagePolicy"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1, "same kind+id must replace, not append");
        assert_eq!(rules[0]["onLow"], "ask");

        let err = handle_set_usage_rule(dir.path(), None, None, "wait").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("--kind") || msg.contains("--id"), "{msg}");

        let err = handle_set_usage_rule(dir.path(), Some("weekly_scoped"), None, "nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("wait"), "must name accepted set: {msg}");
        assert!(msg.contains("unavailable"), "{msg}");
    }

    #[test]
    fn set_tier_fallback_high_with_flags_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"embeddingModel":"x"}"#,
        )
        .unwrap();
        handle_set_tier_fallback(dir.path(), "high", true, true).unwrap();
        let raw = read_raw(dir.path());
        assert!(raw.contains("embeddingModel"), "sparse preserve:\n{raw}");
        let v = read_value(dir.path());
        assert_eq!(v["routing"]["tierFallback"]["maxDifficulty"], "high");
        assert_eq!(v["routing"]["tierFallback"]["includeReview"], true);
        assert_eq!(v["routing"]["tierFallback"]["includeForced"], true);
        assert_validates_clean(dir.path());
    }

    #[test]
    fn unset_tier_fallback_writes_json_null_not_key_deletion() {
        let dir = tempfile::tempdir().unwrap();
        handle_set_tier_fallback(dir.path(), "medium", true, false).unwrap();
        handle_unset_tier_fallback(dir.path()).unwrap();
        let raw = read_raw(dir.path());
        assert!(
            raw.contains("\"tierFallback\": null") || raw.contains("\"tierFallback\":null"),
            "must write JSON null, not delete key:\n{raw}"
        );
        let v = read_value(dir.path());
        assert!(
            v["routing"]["tierFallback"].is_null(),
            "tierFallback must be null:\n{v}"
        );
        // Key present (null), not absent.
        assert!(
            v["routing"]
                .as_object()
                .unwrap()
                .contains_key("tierFallback"),
            "key must remain present as null"
        );
    }

    #[test]
    fn set_tier_fallback_typo_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = handle_set_tier_fallback(dir.path(), "hiigh", true, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CONFIG ERROR"), "{msg}");
        assert!(msg.contains("low") && msg.contains("medium") && msg.contains("high"), "{msg}");
    }

    #[test]
    fn show_to_without_opt_in_prints_policy_without_percent_left() {
        // Ensure TASK_MGR_USE_API is off for this process (integration sandbox
        // also clears it; unit tests may inherit a developer export).
        use crate::loop_engine::test_utils::EnvGuard;
        let _guard = EnvGuard::remove("TASK_MGR_USE_API");
        let dir = tempfile::tempdir().unwrap();
        handle_init(dir.path(), false, false).unwrap();
        handle_set_usage_rule(dir.path(), Some("weekly_scoped"), None, "unavailable").unwrap();
        handle_set_tier_fallback(dir.path(), "high", true, false).unwrap();
        let out = show_output(dir.path());
        assert!(out.contains("usagePolicy:"), "policy block missing:\n{out}");
        assert!(out.contains("remainingMinPercent:"), "{out}");
        assert!(out.contains("waitIfResetWithinMinutes:"), "{out}");
        assert!(out.contains("stopIfResetBeyondHours:"), "{out}");
        assert!(out.contains("askTtlMinutes:"), "{out}");
        assert!(out.contains("kind=weekly_scoped"), "{out}");
        assert!(out.contains("onLow=unavailable"), "{out}");
        assert!(out.contains("tierFallback:"), "{out}");
        assert!(out.contains("maxDifficulty=high"), "{out}");
        assert!(
            !out.contains("% left"),
            "offline show must not print remaining percents:\n{out}"
        );

        handle_unset_tier_fallback(dir.path()).unwrap();
        let out2 = show_output(dir.path());
        assert!(
            out2.contains("tierFallback: (unset)"),
            "null must render as (unset):\n{out2}"
        );
        assert!(!out2.contains("% left"), "{out2}");
    }
}
