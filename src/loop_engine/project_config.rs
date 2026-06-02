use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::config_io::{OnCorruptJson, write_config_key_at};
use crate::loop_engine::model::{Provider, parse_config_provider};

/// Configuration for the Grok fallback runner (US-005, FR-006).
///
/// When `enabled = true`, the loop engine promotes tasks to the Grok CLI
/// after the Claude overflow ladder is exhausted (rung 4) or after
/// `runtime_error_threshold` consecutive `RuntimeError` rounds. Absent or
/// `enabled = false` → no change to the existing 4-rung ladder.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FallbackRunnerConfig {
    /// Whether the Grok fallback runner is active. Default: `false`.
    #[serde(default)]
    pub enabled: bool,

    /// LLM provider name. Must be `"grok"` for the xAI Grok CLI.
    /// Default: `"grok"`.
    #[serde(default = "default_fallback_provider")]
    pub provider: String,

    /// Grok model ID passed as `--model`. Default: `"grok-build"`.
    #[serde(default = "default_fallback_model")]
    pub model: String,

    /// Absolute path to the Grok CLI binary. When `None`, the binary is
    /// resolved as `"grok"` on the system PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_binary: Option<String>,

    /// Number of consecutive `RuntimeError` rounds on a task before the
    /// Grok fallback hook fires. Default: `2`.
    #[serde(default = "default_fallback_runtime_error_threshold")]
    pub runtime_error_threshold: u32,
}

impl Default for FallbackRunnerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_fallback_provider(),
            model: default_fallback_model(),
            cli_binary: None,
            runtime_error_threshold: default_fallback_runtime_error_threshold(),
        }
    }
}

/// A provider + optional model pair used as a routing target in `PrimaryRunnerConfig`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunnerSpec {
    /// Provider name (e.g. `"grok"`, `"claude"`).
    pub provider: String,
    /// Model identifier passed to the provider CLI (e.g. `"grok-build"`).
    /// Codex v1 may omit this field to route by explicit provider intent only.
    #[serde(default)]
    pub model: String,
    /// Opt-in: when the route's provider is `"codex"` AND this is `true`, a
    /// Codex RUNTIME failure (not auth) promotes the task to the Claude
    /// runner instead of auto-blocking after `runtimeErrorThreshold` rounds.
    /// One-shot per task — once promoted, normal Claude recovery applies and
    /// the task never returns to Codex. Default: `false` (legacy auto-block).
    /// Ignored on non-codex routes — Claude/Grok runners already have their
    /// own cross-provider promotion paths (`fallbackRunner` / `claudeFallbackModel`).
    #[serde(default, alias = "fallbackToClaude")]
    pub runtime_error_fallback: bool,
}

/// Per-task-type and per-id-prefix routing for the primary runner.
///
/// Routes specific task types (e.g. `"review"`, `"milestone"`) or ID prefixes
/// (e.g. `"REVIEW-"`, `"MILESTONE-"`) to a non-default runner, while all other
/// tasks continue to use the default Claude model/runner.
///
/// Phase 1: schema + serde only — resolution-chain wiring comes in a later phase.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryRunnerConfig {
    /// Claude model to fall back to when a task routed via `primaryRunner`
    /// experiences a `RuntimeError`. `None` → use the project/task default
    /// Claude model.
    #[serde(default)]
    pub claude_fallback_model: Option<String>,

    /// Number of consecutive `RuntimeError` rounds before falling back to
    /// Claude. Default: `2`.
    #[serde(default = "default_primary_runtime_error_threshold")]
    pub runtime_error_threshold: u32,

    /// Task-type → `RunnerSpec` routing map. Absent key → empty map.
    #[serde(default)]
    pub by_task_type: HashMap<String, RunnerSpec>,

    /// Task-ID-prefix → `RunnerSpec` routing map. Absent key → empty map.
    #[serde(default)]
    pub by_id_prefix: HashMap<String, RunnerSpec>,

    /// Task-ID-prefix → baseline capability tier → `RunnerSpec` routing map.
    ///
    /// Used when a task has no explicit `model`, after its baseline Claude
    /// model is known from difficulty/default resolution and normalized into
    /// a provider-neutral tier. Tier keys are validated by
    /// `model::parse_baseline_tier_key`.
    #[serde(default, alias = "byBaselineTier")]
    pub baseline_tier_routes: HashMap<String, HashMap<String, RunnerSpec>>,
}

impl Default for PrimaryRunnerConfig {
    fn default() -> Self {
        Self {
            claude_fallback_model: None,
            runtime_error_threshold: default_primary_runtime_error_threshold(),
            by_task_type: HashMap::new(),
            by_id_prefix: HashMap::new(),
            baseline_tier_routes: HashMap::new(),
        }
    }
}

/// Per-project loop configuration read from `.task-mgr/config.json`.
///
/// Allows projects to extend the default tool allowlist with project-specific
/// tools (e.g., `docker`, `curl`, `./scripts/*`) without modifying the core
/// default. Forward-compatible: unknown fields are silently ignored.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    /// Schema version for forward compatibility.
    #[serde(default = "default_version")]
    #[allow(dead_code)]
    pub version: u32,

    /// Additional tool entries appended to CODING_ALLOWED_TOOLS.
    /// Example: `["Bash(docker:*)", "Bash(curl:*)"]`
    #[serde(default)]
    pub additional_allowed_tools: Vec<String>,

    /// Permission mode override for this project.
    /// Values: `"dangerous"`, `"scoped"`, `"auto"`.
    /// When set, overrides the default `Dangerous` mode (env vars still win).
    /// Set to `"scoped"` or `"auto"` to opt this project back into permission
    /// prompts / allowlist enforcement.
    #[serde(default)]
    pub permission_mode: Option<String>,

    /// Ollama server URL for embedding generation.
    /// Defaults to `http://localhost:11435` (the bundled docker-compose stack
    /// uses 11435 to avoid clashing with a host-installed `ollama serve`).
    #[serde(default)]
    pub ollama_url: Option<String>,

    /// Embedding model name for Ollama.
    /// Defaults to `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0`.
    #[serde(default)]
    pub embedding_model: Option<String>,

    /// Claude model to use for `curate dedup` LLM calls.
    /// Defaults to `"haiku"` (latest Haiku via CLI alias).
    #[serde(default)]
    pub dedup_model: Option<String>,

    /// Per-project default Claude model. Falls below `prd_metadata.default_model`
    /// and above `user_config.default_model` in the resolution chain (see
    /// `loop_engine::model::resolve_task_model`).
    #[serde(default)]
    pub default_model: Option<String>,

    /// llama-box reranker endpoint. Must be set together with `reranker_model`;
    /// if only one is present the reranker is disabled with a warning.
    #[serde(default)]
    pub reranker_url: Option<String>,

    /// Cross-encoder model name served by the llama-box `/v1/rerank` endpoint.
    #[serde(default)]
    pub reranker_model: Option<String>,

    /// How many candidates per backend to fetch before reranking.
    /// Defaults to 3 when unset; values of 0 are clamped to 1 with a warning.
    #[serde(default)]
    pub reranker_over_fetch: Option<u32>,

    /// Hard cap (seconds) on a single parallel-slot merge-conflict resolution
    /// Claude run. Defaults to 600 (10 min). Lift for projects with large
    /// merges; lower for tight feedback loops.
    #[serde(default)]
    pub merge_resolver_timeout_secs: Option<u64>,

    /// `--effort` value passed to Claude when resolving a parallel-slot merge
    /// conflict. Defaults to `"medium"`. Use `"high"` for cross-cutting
    /// refactors that conflict on semantic logic.
    #[serde(default)]
    pub merge_resolver_effort: Option<String>,

    /// Halt the loop after this many *consecutive* parallel-slot merge-back
    /// failure waves. Default: `2` — a single failed merge is recoverable
    /// (next wave gets a clean slate from the resolver), but two in a row
    /// indicate a cascading state where letting more waves run risks the
    /// kind of branch divergence the mw-datalake incident produced.
    ///
    /// Threshold semantics:
    /// - `0` — never halt (legacy "log and continue" behavior preserved bit-for-bit)
    /// - `1` — halt on any merge-back failure
    /// - `2` (default) — halt after two consecutive merge-back failure waves
    #[serde(default = "default_merge_fail_halt_threshold")]
    pub merge_fail_halt_threshold: u32,

    /// Project-level extension to the baseline `IMPLICIT_OVERLAP_FILES` list
    /// used by `select_parallel_group` (FEAT-003). Match is by basename across
    /// any path in a task's `touchesFiles`. Extends rather than replaces the
    /// baseline so users opt IN to extra shared-infra files (e.g. an in-house
    /// `gradle-wrapper.lock`) without losing the language defaults.
    #[serde(default)]
    pub implicit_overlap_files: Vec<String>,

    /// Maximum number of stash-pop conflicts per slot per run before the slot
    /// is demoted to `failed_slots(PreResolver)` and the consecutive-merge-fail
    /// halt threshold trips. Controlled by the bounded warn-and-continue policy
    /// in `cleanup_preparation` (FEAT-003).
    ///
    /// Threshold semantics:
    /// - `0` — never halt on stash-pop conflicts (matches `merge_fail_halt_threshold == 0`)
    /// - `5` (default) — halt after 5 stash-pop conflict events on the same slot
    #[allow(dead_code)]
    #[serde(default = "default_slot_stash_limit")]
    pub slot_stash_limit: u32,

    /// Whether to auto-launch `/review-loop` after a successful loop/batch run.
    /// Default: `true`. Set to `false` to suppress the interactive review session.
    /// CLI flags `--auto-review` / `--no-auto-review` override this value.
    #[serde(default = "default_auto_review")]
    pub auto_review: bool,

    /// Minimum number of completed tasks required to trigger auto-review.
    /// Runs that completed fewer than this many tasks are not reviewed automatically.
    /// Default: `3`.
    #[serde(default = "default_auto_review_min_tasks")]
    pub auto_review_min_tasks: u32,

    /// Grok fallback runner configuration. Absent key → `None`; explicit
    /// `null` → `None`; explicit object → `Some` (with per-field defaults
    /// applied for any omitted optional fields). Default: `None`.
    #[serde(default)]
    pub fallback_runner: Option<FallbackRunnerConfig>,

    /// Optional model to route review-class tasks to (`CODE-REVIEW-*`,
    /// `MILESTONE-FINAL`, `REVIEW-*`). When set, these tasks are dispatched
    /// using this model instead of the default Claude model. Typically a Grok
    /// model id (e.g. `"grok-build"`). Absent key or explicit `null` → `None`.
    #[serde(default)]
    pub review_model: Option<String>,

    /// Primary runner routing configuration. Absent key → `None`; explicit
    /// `null` → `None`; explicit object → `Some` (with per-field defaults
    /// applied for any omitted optional fields). Default: `None`.
    #[serde(default)]
    pub primary_runner: Option<PrimaryRunnerConfig>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            version: 1,
            additional_allowed_tools: Vec::new(),
            permission_mode: None,
            ollama_url: None,
            embedding_model: None,
            dedup_model: None,
            default_model: None,
            reranker_url: None,
            reranker_model: None,
            reranker_over_fetch: None,
            merge_resolver_timeout_secs: None,
            merge_resolver_effort: None,
            merge_fail_halt_threshold: default_merge_fail_halt_threshold(),
            implicit_overlap_files: Vec::new(),
            slot_stash_limit: default_slot_stash_limit(),
            auto_review: default_auto_review(),
            auto_review_min_tasks: default_auto_review_min_tasks(),
            fallback_runner: None,
            review_model: None,
            primary_runner: None,
        }
    }
}

impl ProjectConfig {
    /// Returns `Some((url, model, over_fetch))` only when both `reranker_url`
    /// AND `reranker_model` are set. Returns `None` silently when neither is
    /// set; warns and returns `None` when exactly one is present.
    pub fn resolved_reranker_config(&self) -> Option<(String, String, u32)> {
        match (&self.reranker_url, &self.reranker_model) {
            (Some(url), Some(model)) => {
                let over_fetch = match self.reranker_over_fetch {
                    None => 3,
                    Some(0) => {
                        crate::output::warn("rerankerOverFetch=0 is invalid; clamping to 1");
                        1
                    }
                    Some(n) => n,
                };
                Some((url.clone(), model.clone(), over_fetch))
            }
            (None, None) => None,
            _ => {
                crate::output::warn(
                    "rerankerUrl/rerankerModel: both must be set; reranker disabled",
                );
                None
            }
        }
    }
}

fn default_version() -> u32 {
    1
}

/// Default consecutive-merge-fail threshold (2). Single failures are recoverable;
/// two-in-a-row indicate a cascade.
fn default_merge_fail_halt_threshold() -> u32 {
    2
}

/// Default per-slot per-run stash-pop conflict limit (5).
fn default_slot_stash_limit() -> u32 {
    5
}

/// Auto-review is enabled by default.
fn default_auto_review() -> bool {
    true
}

/// Minimum completed tasks before auto-review fires (default 3).
fn default_auto_review_min_tasks() -> u32 {
    3
}

/// Default provider name for the Grok fallback runner (PRD §6).
fn default_fallback_provider() -> String {
    "grok".to_string()
}

/// Default model ID for the Grok fallback runner (PRD §6).
fn default_fallback_model() -> String {
    "grok-build".to_string()
}

/// Default consecutive-RuntimeError threshold before Grok fallback fires (PRD §3 US-005).
fn default_fallback_runtime_error_threshold() -> u32 {
    2
}

/// Default consecutive-RuntimeError threshold before primary-runner falls back to Claude.
fn default_primary_runtime_error_threshold() -> u32 {
    2
}

/// Check that `path` exists, is a regular file, and (on Unix) has the
/// executable bit set for some user class. Spawn will only succeed against
/// an executable file, so the startup probe should reject non-executable
/// paths up-front rather than letting them fail with a less-helpful
/// `std::io::Error` at first promotion. On non-Unix targets, falls back to
/// `exists()` (no mode bits available).
fn is_executable_path(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Resolve and probe the Grok binary path.
///
/// Resolution order (matches `runner::resolve_grok_binary`):
/// 1. `GROK_BINARY` env var when set AND non-empty/non-whitespace.
/// 2. `fallback_cli_binary` when set AND non-empty/non-whitespace — probed verbatim.
/// 3. Bare name `"grok"` — searches PATH directories.
///
/// Returns `Ok(())` when the resolved binary is an executable file.
/// Returns `Err(binary_name)` when it is missing or not executable.
///
/// Empty/whitespace values fall through to the next link — common shell
/// footgun (`export GROK_BINARY=""` must not cause a misleading failure
/// when grok is on PATH).
fn resolve_and_verify_grok_binary(fallback_cli_binary: Option<&str>) -> Result<(), String> {
    let env_bin = std::env::var("GROK_BINARY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let cli_bin = fallback_cli_binary
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string);

    let (binary, found) = if let Some(env_bin) = env_bin {
        let exec = is_executable_path(std::path::Path::new(&env_bin));
        (env_bin, exec)
    } else if let Some(explicit) = cli_bin {
        let exec = is_executable_path(std::path::Path::new(&explicit));
        (explicit, exec)
    } else {
        let name = "grok";
        let found = std::env::var_os("PATH")
            .map(|path_var| {
                std::env::split_paths(&path_var).any(|dir| is_executable_path(&dir.join(name)))
            })
            .unwrap_or(false);
        (name.to_string(), found)
    };

    if found { Ok(()) } else { Err(binary) }
}

fn resolve_and_verify_codex_binary() -> Result<(), String> {
    let env_bin = std::env::var("CODEX_BINARY")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let (binary, found) = if let Some(env_bin) = env_bin {
        let exec = is_executable_path(std::path::Path::new(&env_bin));
        (env_bin, exec)
    } else {
        let name = "codex";
        let found = std::env::var_os("PATH")
            .map(|path_var| {
                std::env::split_paths(&path_var).any(|dir| is_executable_path(&dir.join(name)))
            })
            .unwrap_or(false);
        (name.to_string(), found)
    };

    if found { Ok(()) } else { Err(binary) }
}

/// Verify that the Grok fallback binary is reachable at loop startup.
///
/// Returns `Ok(())` when `cfg` is `None` or `cfg.enabled` is `false`.
/// Returns `Err` when `cfg.enabled` is `true` and the binary is missing or
/// not executable. The error names the binary for operator diagnostics.
pub fn check_fallback_runner_binary(cfg: Option<&FallbackRunnerConfig>) -> TaskMgrResult<()> {
    let cfg = match cfg {
        None => return Ok(()),
        Some(c) if !c.enabled => return Ok(()),
        Some(c) => c,
    };
    resolve_and_verify_grok_binary(cfg.cli_binary.as_deref()).map_err(|binary| {
        TaskMgrError::NotFound {
            resource_type: "Fallback runner binary".to_string(),
            id: format!(
                "{binary} — install the Grok CLI or set `fallbackRunner.cliBinary` to the \
                 correct path (must be an executable file), then retry"
            ),
        }
    })
}

pub fn validate_runner_routing_config(cfg: &ProjectConfig) -> TaskMgrResult<()> {
    if let Some(fallback) = cfg.fallback_runner.as_ref()
        && fallback.enabled
        && !fallback.provider.trim().eq_ignore_ascii_case("grok")
    {
        return Err(TaskMgrError::InvalidConfig {
            field: "fallbackRunner.provider".to_string(),
            message: "v1 fallbackRunner only supports provider \"grok\"".to_string(),
        });
    }
    if let Some(primary) = cfg.primary_runner.as_ref() {
        for (map_name, key, spec) in primary_runner_specs(primary) {
            if spec.provider.trim().is_empty() {
                return Err(TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.provider"),
                    message: "provider must not be blank".to_string(),
                });
            }
            // Strict parse: surface the error from `parse_config_provider`
            // (typos like "openai" / "codex-cli" / "groq") so a misspelled
            // provider hard-fails at validation instead of silently routing
            // the task to Claude. Returning `Option::None` from a parser is
            // the exact silent-fallback footgun this branch defends against.
            let provider = parse_config_provider(&spec.provider).map_err(|message| {
                TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.provider"),
                    message,
                }
            })?;
            if spec.model.trim().is_empty() && provider != Provider::Codex {
                return Err(TaskMgrError::InvalidConfig {
                    field: format!("primaryRunner.{map_name}.{key}.model"),
                    message: "model must not be blank unless provider is codex".to_string(),
                });
            }
        }
        for (prefix, tier_map) in &primary.baseline_tier_routes {
            for tier_key in tier_map.keys() {
                crate::loop_engine::model::parse_baseline_tier_key(tier_key).map_err(
                    |message| TaskMgrError::InvalidConfig {
                        field: format!("primaryRunner.baselineTierRoutes.{prefix}.{tier_key}"),
                        message,
                    },
                )?;
            }
        }
        if cfg
            .review_model
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && primary_runner_contains_codex_review_route(primary)
        {
            return Err(TaskMgrError::InvalidConfig {
                field: "reviewModel".to_string(),
                message: "reviewModel is string-only in v1 and overrides primaryRunner Codex review routes; unset reviewModel or remove the Codex review route".to_string(),
            });
        }
    }
    Ok(())
}

pub fn check_codex_runner_binary(primary: Option<&PrimaryRunnerConfig>) -> TaskMgrResult<()> {
    let Some(primary) = primary else {
        return Ok(());
    };
    if !primary_runner_specs(primary)
        .any(|(_, _, spec)| parse_config_provider(&spec.provider).ok() == Some(Provider::Codex))
    {
        return Ok(());
    }
    resolve_and_verify_codex_binary().map_err(|binary| TaskMgrError::NotFound {
        resource_type: "Codex CLI binary required by primaryRunner".to_string(),
        id: format!(
            "{binary} — install Codex CLI or set CODEX_BINARY to an executable file, then retry"
        ),
    })
}

fn primary_runner_specs(
    primary: &PrimaryRunnerConfig,
) -> impl Iterator<Item = (&'static str, String, &RunnerSpec)> {
    primary
        .by_task_type
        .iter()
        .map(|(k, v)| ("byTaskType", k.clone(), v))
        .chain(
            primary
                .by_id_prefix
                .iter()
                .map(|(k, v)| ("byIdPrefix", k.clone(), v)),
        )
        .chain(
            primary
                .baseline_tier_routes
                .iter()
                .flat_map(|(prefix, tiers)| {
                    tiers.iter().map(move |(tier, spec)| {
                        ("baselineTierRoutes", format!("{prefix}.{tier}"), spec)
                    })
                }),
        )
}

fn primary_runner_contains_codex_review_route(primary: &PrimaryRunnerConfig) -> bool {
    primary_runner_specs(primary).any(|(map_name, key, spec)| {
        parse_config_provider(&spec.provider).ok() == Some(Provider::Codex)
            && match map_name {
                "byTaskType" => {
                    key.eq_ignore_ascii_case("review")
                        || key.eq_ignore_ascii_case("code-review")
                        || key.eq_ignore_ascii_case("milestone-final")
                }
                "byIdPrefix" => {
                    let k = key.trim_end_matches('-').to_ascii_uppercase();
                    matches!(k.as_str(), "CODE-REVIEW" | "REVIEW" | "MILESTONE-FINAL")
                }
                "baselineTierRoutes" => {
                    let prefix = key
                        .split_once('.')
                        .map(|(prefix, _)| prefix)
                        .unwrap_or(key.as_str());
                    let k = prefix.trim_end_matches('-').to_ascii_uppercase();
                    matches!(k.as_str(), "CODE-REVIEW" | "REVIEW" | "MILESTONE-FINAL")
                }
                _ => false,
            }
    })
}

/// Verify that the Grok binary is reachable when `reviewModel` routes to Grok.
///
/// Returns `Ok(())` when `review_model` is `None` or resolves to a non-Grok
/// provider. Returns `Err` when it resolves to Grok and the binary is missing
/// or not executable. The error names the model and binary for diagnostics.
pub fn check_review_model_binary(
    review_model: Option<&str>,
    fallback_cli_binary: Option<&str>,
) -> TaskMgrResult<()> {
    use crate::loop_engine::model::{Provider, provider_for_model};

    if provider_for_model(review_model) != Provider::Grok {
        return Ok(());
    }
    resolve_and_verify_grok_binary(fallback_cli_binary).map_err(|binary| TaskMgrError::NotFound {
        resource_type: "Grok CLI binary required by reviewModel".to_string(),
        id: format!(
            "{binary} — install the Grok CLI or set `fallbackRunner.cliBinary` to the \
                 correct path, then run `grok login` to authenticate \
                 (reviewModel = {rm})",
            rm = review_model.unwrap_or("<unknown>")
        ),
    })
}

/// Startup pre-flight: validate the project config, then probe every runner
/// binary the config will need, BEFORE the first iteration.
///
/// This is the single source of truth for "is this project safe to run?" and
/// MUST be called from every loop entry point — both `loop run` (single PRD)
/// and `batch run` (N PRDs). Hoisting it here closes the parity gap where a
/// misconfigured provider string or a missing `codex`/`grok` binary would
/// surface only on `loop run`, but run unvalidated under `batch run`.
///
/// Ordering matches `loop run`'s historical block: validation runs BEFORE the
/// binary probes so an operator who mis-typed a provider string sees the
/// structured config error, not a misleading "binary missing" message from a
/// downstream probe that wouldn't have fired anyway.
///
/// Codex binary probe is route-gated by `check_codex_runner_binary`: a
/// pure-Claude / pure-Grok project triggers no PATH lookup for `codex`.
///
/// Failure semantics for `batch run`: a failure here aborts the WHOLE batch
/// before any PRD runs. Config validity and binary availability are
/// project-level (every PRD in the batch shares the same `.task-mgr/config.json`
/// and `$PATH`), so a failure affects every PRD equally — failing fast up-front
/// mirrors `loop run`'s fail-before-iteration-1 contract and avoids burning N
/// partial runs on a uniformly-broken environment.
pub fn preflight_validate_and_probe(cfg: &ProjectConfig) -> TaskMgrResult<()> {
    validate_runner_routing_config(cfg)?;
    check_fallback_runner_binary(cfg.fallback_runner.as_ref())?;
    check_review_model_binary(
        cfg.review_model.as_deref(),
        cfg.fallback_runner
            .as_ref()
            .and_then(|fr| fr.cli_binary.as_deref()),
    )?;
    check_codex_runner_binary(cfg.primary_runner.as_ref())?;
    Ok(())
}

/// Read project config from `<db_dir>/config.json`.
///
/// Returns default (empty) config if the file doesn't exist.
/// Warns on invalid JSON but does not fail — returns defaults instead.
pub fn read_project_config(db_dir: &Path) -> ProjectConfig {
    let path = db_dir.join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let mut value: serde_json::Value = match serde_json::from_str(&contents) {
                Ok(value) => value,
                Err(e) => {
                    crate::output::warn(&format!("Invalid .task-mgr/config.json: {e}"));
                    return ProjectConfig::default();
                }
            };
            // Normalize legacy routing keys in place, then deserialize from the
            // SAME migrated value we just normalized. Deserializing the original
            // would rely on serde aliases covering every rewrite — a silent
            // wrong-value risk the moment a future legacy->canonical rename lands
            // in migrate_project_config_value without a matching alias.
            if migrate_project_config_value(&mut value) {
                crate::output::warn(
                    ".task-mgr/config.json uses legacy model routing keys; run `task-mgr init` \
                     to update it to baselineTierRoutes/runtimeErrorFallback",
                );
            }
            serde_json::from_value(value).unwrap_or_else(|e| {
                crate::output::warn(&format!("Invalid .task-mgr/config.json: {e}"));
                ProjectConfig::default()
            })
        }
        Err(_) => ProjectConfig::default(),
    }
}

/// Rewrite `<db_dir>/config.json` from legacy routing keys to the current
/// canonical shape. Returns `true` when the file was changed.
pub fn update_project_config_format(db_dir: &Path) -> std::io::Result<bool> {
    use std::io::Write;

    let path = db_dir.join("config.json");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) if !contents.trim().is_empty() => contents,
        _ => return Ok(false),
    };
    let mut value: serde_json::Value = serde_json::from_str(&contents).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: malformed JSON: {e}", path.display()),
        )
    })?;
    if !value.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: config is not a JSON object", path.display()),
        ));
    }
    if !migrate_project_config_value(&mut value) {
        return Ok(false);
    }

    let contents = serde_json::to_string_pretty(&value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let dir = path.parent().unwrap_or(db_dir);
    let mut tmp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".json")
        .tempfile_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.write_all(b"\n")?;
    // The tempfile is created 0o600; persist preserves that, which would narrow
    // an originally group/world-readable config. Re-apply the original file's
    // mode before persisting so the migration is permission-neutral.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(orig) = std::fs::metadata(&path) {
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(orig.permissions().mode()))?;
        }
    }
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(true)
}

fn migrate_project_config_value(value: &mut serde_json::Value) -> bool {
    let Some(root) = value.as_object_mut() else {
        return false;
    };
    let Some(primary) = root
        .get_mut("primaryRunner")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };

    let mut changed = false;
    changed |= migrate_runner_spec_map(primary.get_mut("byTaskType"));
    changed |= migrate_runner_spec_map(primary.get_mut("byIdPrefix"));

    if let Some(legacy) = primary.remove("byBaselineTier") {
        changed = true;
        merge_baseline_tier_routes(primary, legacy);
    }
    changed |= migrate_baseline_tier_routes(primary.get_mut("baselineTierRoutes"));
    changed
}

/// Fold a legacy `byBaselineTier` map into the canonical `baselineTierRoutes`.
/// On collision (same prefix + same canonical tier key) the existing canonical
/// entry wins — legacy values only fill gaps (`entry().or_insert`), never
/// overwrite a route the operator already expressed in the canonical form.
fn merge_baseline_tier_routes(
    primary: &mut serde_json::Map<String, serde_json::Value>,
    legacy: serde_json::Value,
) {
    let canonical = primary
        .entry("baselineTierRoutes".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(canonical_obj) = canonical.as_object_mut() else {
        return;
    };
    let serde_json::Value::Object(legacy_prefixes) = legacy else {
        return;
    };
    for (prefix, legacy_tiers) in legacy_prefixes {
        let target = canonical_obj
            .entry(prefix)
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let Some(target_tiers) = target.as_object_mut() else {
            continue;
        };
        let serde_json::Value::Object(legacy_tiers) = legacy_tiers else {
            continue;
        };
        for (tier, spec) in legacy_tiers {
            target_tiers
                .entry(canonical_baseline_tier_key(&tier).to_string())
                .or_insert(spec);
        }
    }
}

fn migrate_baseline_tier_routes(value: Option<&mut serde_json::Value>) -> bool {
    let Some(prefixes) = value.and_then(serde_json::Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for tiers in prefixes.values_mut() {
        let Some(tiers) = tiers.as_object_mut() else {
            continue;
        };
        let old = std::mem::take(tiers);
        for (tier, mut spec) in old {
            let canonical = canonical_baseline_tier_key(&tier).to_string();
            if canonical != tier {
                changed = true;
            }
            changed |= migrate_runner_spec(&mut spec);
            tiers.entry(canonical).or_insert(spec);
        }
    }
    changed
}

fn migrate_runner_spec_map(value: Option<&mut serde_json::Value>) -> bool {
    let Some(map) = value.and_then(serde_json::Value::as_object_mut) else {
        return false;
    };
    // migrate_runner_spec mutates each spec; run it on EVERY entry (no `.any()`
    // short-circuit, which would stop migrating after the first hit).
    #[allow(clippy::unnecessary_fold)]
    map.values_mut()
        .fold(false, |changed, spec| migrate_runner_spec(spec) || changed)
}

fn migrate_runner_spec(spec: &mut serde_json::Value) -> bool {
    let Some(obj) = spec.as_object_mut() else {
        return false;
    };
    let Some(legacy) = obj.remove("fallbackToClaude") else {
        return false;
    };
    obj.entry("runtimeErrorFallback".to_string())
        .or_insert(legacy);
    true
}

fn canonical_baseline_tier_key(key: &str) -> &str {
    match key.trim().to_ascii_lowercase().as_str() {
        "haiku" => "low",
        "sonnet" => "standard",
        "opus" => "high",
        "low" => "low",
        "standard" => "standard",
        "high" => "high",
        _ => key,
    }
}

/// Set (or clear) the `defaultModel` field in `<db_dir>/config.json` without
/// clobbering other fields — including unknown forward-compat ones.
///
/// Pass `Some(model)` to set, `None` to remove the key entirely.
/// Creates the file and parent dir if needed. Writes atomically via a
/// same-directory tempfile + rename so readers never see a half-written JSON.
/// Tolerant: malformed JSON is silently replaced by the `{"version":1}` seed.
pub fn write_default_model(db_dir: &Path, model: Option<&str>) -> std::io::Result<()> {
    let path = db_dir.join("config.json");
    write_config_key_at(
        &path,
        "defaultModel",
        model.map(|m| serde_json::Value::String(m.to_string())),
        serde_json::json!({ "version": 1 }),
        OnCorruptJson::UseSeed,
    )
}

/// Set (or clear) the `reviewModel` field in `<db_dir>/config.json` without
/// clobbering other fields.
///
/// Pass `Some(model)` to set, `None` to remove the key.
/// Creates the file if absent (`{"version":1}` + the target key).
/// Returns `Err` if the existing file contains malformed JSON (path named in message).
pub fn write_review_model(db_dir: &Path, model: Option<&str>) -> std::io::Result<()> {
    let path = db_dir.join("config.json");
    write_config_key_at(
        &path,
        "reviewModel",
        model.map(|m| serde_json::Value::String(m.to_string())),
        serde_json::json!({ "version": 1 }),
        OnCorruptJson::ReturnError,
    )
}

/// Set (or clear) the `fallbackRunner` block in `<db_dir>/config.json` without
/// clobbering other fields.
///
/// Pass `Some(cfg)` to set, `None` to remove the key.
/// Creates the file if absent (`{"version":1}` + the target key).
/// Returns `Err` if the existing file contains malformed JSON (path named in message).
pub fn write_fallback_runner(
    db_dir: &Path,
    cfg: Option<&FallbackRunnerConfig>,
) -> std::io::Result<()> {
    let path = db_dir.join("config.json");
    let v = cfg
        .map(|c| {
            serde_json::to_value(c)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })
        .transpose()?;
    write_config_key_at(
        &path,
        "fallbackRunner",
        v,
        serde_json::json!({ "version": 1 }),
        OnCorruptJson::ReturnError,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_read_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert!(config.additional_allowed_tools.is_empty());
    }

    #[test]
    fn test_read_invalid_json_warns_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "not json").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert!(config.additional_allowed_tools.is_empty());
    }

    #[test]
    fn test_read_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "additionalAllowedTools": ["Bash(docker:*)", "Bash(curl:*)"]}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert_eq!(
            config.additional_allowed_tools,
            vec!["Bash(docker:*)", "Bash(curl:*)"]
        );
    }

    #[test]
    fn test_read_config_with_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "additionalAllowedTools": ["Bash(docker:*)"], "futureField": true}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.additional_allowed_tools, vec!["Bash(docker:*)"]);
    }

    #[test]
    fn test_default_version() {
        let config = ProjectConfig::default();
        assert_eq!(config.version, 1);
    }

    #[test]
    fn test_empty_json_object_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.version, 1);
        assert!(config.additional_allowed_tools.is_empty());
        assert!(config.permission_mode.is_none());
    }

    #[test]
    fn test_permission_mode_dangerous() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"permissionMode": "dangerous"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.permission_mode.as_deref(), Some("dangerous"));
    }

    #[test]
    fn test_permission_mode_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"additionalAllowedTools": []}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.permission_mode.is_none());
    }

    #[test]
    fn test_ollama_url_and_embedding_model() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"ollamaUrl": "http://gpu-server:11434", "embeddingModel": "custom-model"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(
            config.ollama_url.as_deref(),
            Some("http://gpu-server:11434")
        );
        assert_eq!(config.embedding_model.as_deref(), Some("custom-model"));
    }

    #[test]
    fn test_ollama_url_and_embedding_model_default_to_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.ollama_url.is_none());
        assert!(config.embedding_model.is_none());
    }

    #[test]
    fn test_default_model_reads() {
        use crate::loop_engine::model::SONNET_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(r#"{{"defaultModel": "{SONNET_MODEL}"}}"#),
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.default_model.as_deref(), Some(SONNET_MODEL));
    }

    #[test]
    fn test_write_default_model_creates_file() {
        use crate::loop_engine::model::OPUS_MODEL;
        let dir = tempfile::tempdir().unwrap();
        write_default_model(dir.path(), Some(OPUS_MODEL)).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.default_model.as_deref(), Some(OPUS_MODEL));
    }

    #[test]
    fn test_write_default_model_preserves_unknown_fields() {
        use crate::loop_engine::model::HAIKU_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "futureField": {"nested": true}, "additionalAllowedTools": ["Bash(docker:*)"]}"#,
        )
        .unwrap();
        write_default_model(dir.path(), Some(HAIKU_MODEL)).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("futureField"),
            "unknown field must survive the write"
        );
        assert!(raw.contains("nested"));
        assert!(raw.contains("Bash(docker:*)"));
        assert!(raw.contains(HAIKU_MODEL));
    }

    #[test]
    fn test_write_default_model_removes_key_when_none() {
        use crate::loop_engine::model::OPUS_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(r#"{{"defaultModel": "{OPUS_MODEL}", "version": 1}}"#),
        )
        .unwrap();
        write_default_model(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("defaultModel"),
            "key should be removed, got: {raw}"
        );
    }

    #[test]
    fn test_resolved_reranker_config_both_set() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: Some("m".to_string()),
            reranker_over_fetch: Some(5),
            ..Default::default()
        };
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 5))
        );
    }

    #[test]
    fn test_resolved_reranker_config_default_over_fetch() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: Some("m".to_string()),
            reranker_over_fetch: None,
            ..Default::default()
        };
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 3))
        );
    }

    #[test]
    fn test_resolved_reranker_config_over_fetch_zero_clamped_to_one() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: Some("m".to_string()),
            reranker_over_fetch: Some(0),
            ..Default::default()
        };
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 1))
        );
    }

    #[test]
    fn test_resolved_reranker_config_only_url_set() {
        let config = ProjectConfig {
            reranker_url: Some("http://x".to_string()),
            reranker_model: None,
            ..Default::default()
        };
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_resolved_reranker_config_only_model_set() {
        let config = ProjectConfig {
            reranker_url: None,
            reranker_model: Some("m".to_string()),
            ..Default::default()
        };
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_resolved_reranker_config_neither_set() {
        let config = ProjectConfig::default();
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_resolved_reranker_config_from_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.resolved_reranker_config().is_none());
    }

    #[test]
    fn test_reranker_config_deserializes_from_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"rerankerUrl":"http://x","rerankerModel":"m","rerankerOverFetch":5}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.reranker_url.as_deref(), Some("http://x"));
        assert_eq!(config.reranker_model.as_deref(), Some("m"));
        assert_eq!(config.reranker_over_fetch, Some(5));
        assert_eq!(
            config.resolved_reranker_config(),
            Some(("http://x".to_string(), "m".to_string(), 5))
        );
    }

    #[test]
    fn test_merge_fail_halt_threshold_default_is_two() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 2);
    }

    #[test]
    fn test_merge_fail_halt_threshold_default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 2);
    }

    #[test]
    fn test_merge_fail_halt_threshold_can_be_zero() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"mergeFailHaltThreshold": 0}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 0);
    }

    #[test]
    fn test_merge_fail_halt_threshold_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"mergeFailHaltThreshold": 5}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.merge_fail_halt_threshold, 5);
    }

    #[test]
    fn test_default_struct_has_threshold_two() {
        let config = ProjectConfig::default();
        assert_eq!(config.merge_fail_halt_threshold, 2);
    }

    #[test]
    fn test_implicit_overlap_files_default_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.implicit_overlap_files.is_empty());
    }

    #[test]
    fn test_implicit_overlap_files_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"implicitOverlapFiles": ["custom.lock", "gradle-wrapper.lock"]}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(
            config.implicit_overlap_files,
            vec!["custom.lock".to_string(), "gradle-wrapper.lock".to_string()]
        );
    }

    #[test]
    fn test_implicit_overlap_files_default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.implicit_overlap_files.is_empty());
    }

    #[test]
    fn test_write_default_model_recovers_from_corrupt_json() {
        use crate::loop_engine::model::SONNET_MODEL;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "not json at all").unwrap();
        // Write recovers by starting fresh — doesn't lose data that wasn't parseable anyway.
        write_default_model(dir.path(), Some(SONNET_MODEL)).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.default_model.as_deref(), Some(SONNET_MODEL));
    }

    #[test]
    fn test_slot_stash_limit_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"slotStashLimit":10}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.slot_stash_limit, 10);
    }

    #[test]
    fn test_slot_stash_limit_default_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.slot_stash_limit, 5);
    }

    #[test]
    fn test_slot_stash_limit_accepts_zero() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"slotStashLimit":0}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.slot_stash_limit, 0);
    }

    #[test]
    fn test_slot_stash_limit_default_struct() {
        let config = ProjectConfig::default();
        assert_eq!(config.slot_stash_limit, 5);
    }

    #[test]
    fn test_auto_review_default_is_true() {
        // Default impl
        assert!(ProjectConfig::default().auto_review);

        // Missing file → defaults
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review);

        // Empty JSON → serde default fn fires (not bool's Default::default())
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review);
    }

    #[test]
    fn test_auto_review_min_tasks_default_is_three() {
        // Default impl
        assert_eq!(ProjectConfig::default().auto_review_min_tasks, 3);

        // Missing file → defaults
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.auto_review_min_tasks, 3);

        // Empty JSON → serde default fn fires
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.auto_review_min_tasks, 3);
    }

    #[test]
    fn test_review_model_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.review_model.is_none());
    }

    #[test]
    fn test_review_model_default_impl_is_none() {
        assert!(ProjectConfig::default().review_model.is_none());
    }

    #[test]
    fn test_review_model_deserializes_from_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"reviewModel": "grok-build"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.review_model.as_deref(), Some("grok-build"));
    }

    #[test]
    fn test_review_model_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = read_project_config(dir.path());
        assert!(config.review_model.is_none());
    }

    #[test]
    fn test_review_model_snake_case_not_accepted() {
        // Wire name is camelCase; snake_case must not set the field.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"review_model": "grok-build"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(
            config.review_model.is_none(),
            "snake_case key must not set review_model"
        );
    }

    #[test]
    fn test_auto_review_round_trips_from_json() {
        let dir = tempfile::tempdir().unwrap();

        // Explicit false + explicit min_tasks
        fs::write(
            dir.path().join("config.json"),
            r#"{"autoReview": false, "autoReviewMinTasks": 0}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(!config.auto_review);
        assert_eq!(config.auto_review_min_tasks, 0);

        // Only autoReview=false, min_tasks stays at default
        fs::write(dir.path().join("config.json"), r#"{"autoReview": false}"#).unwrap();
        let config = read_project_config(dir.path());
        assert!(!config.auto_review);
        assert_eq!(config.auto_review_min_tasks, 3);

        // Only autoReviewMinTasks=5, auto_review stays at default true
        fs::write(
            dir.path().join("config.json"),
            r#"{"autoReviewMinTasks": 5}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review);
        assert_eq!(config.auto_review_min_tasks, 5);

        // snake_case keys are rejected — field stays at default true
        fs::write(dir.path().join("config.json"), r#"{"auto_review": false}"#).unwrap();
        let config = read_project_config(dir.path());
        assert!(config.auto_review, "snake_case key must not set the field");
    }

    // ---- check_review_model_binary tests ----

    #[test]
    fn test_check_review_model_binary_claude_provider_is_noop() {
        use crate::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};
        // When reviewModel resolves to Claude, the probe must succeed regardless
        // of whether grok is on PATH — no PATH lookup must occur.
        assert!(check_review_model_binary(Some(OPUS_MODEL), None).is_ok());
        assert!(check_review_model_binary(Some(SONNET_MODEL), None).is_ok());
    }

    #[test]
    fn test_check_review_model_binary_none_is_noop() {
        // Unset reviewModel → probe is always a no-op.
        assert!(check_review_model_binary(None, None).is_ok());
    }

    #[test]
    fn test_check_review_model_binary_grok_missing_binary_errors() {
        // reviewModel resolves to Grok AND the binary is absent → Err.
        // Inject a path that definitely doesn't exist as GROK_BINARY so the
        // probe never falls through to the system PATH. The mutex serializes
        // against other tests that mutate the GROK_BINARY env var.
        use crate::loop_engine::test_utils::GROK_BINARY_MUTEX;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bogus = "/tmp/task-mgr-test-nonexistent-grok-binary-xyz";
        unsafe { std::env::set_var("GROK_BINARY", bogus) };
        let result = check_review_model_binary(Some("grok-build"), None);
        unsafe { std::env::remove_var("GROK_BINARY") };
        assert!(result.is_err(), "missing grok binary must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("grok-build"),
            "error should mention the reviewModel value; got: {msg}"
        );
    }

    #[test]
    fn test_check_review_model_binary_grok_explicit_missing_cli_binary_errors() {
        // When fallbackRunner.cliBinary is set to a non-existent path AND
        // GROK_BINARY is unset, the probe checks the explicit path and errors.
        use crate::loop_engine::test_utils::GROK_BINARY_MUTEX;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bogus_cli = "/tmp/task-mgr-test-nonexistent-grok-cli-xyz";
        // Ensure GROK_BINARY is absent so we fall through to fallback_cli_binary.
        unsafe { std::env::remove_var("GROK_BINARY") };
        let result = check_review_model_binary(Some("grok-build"), Some(bogus_cli));
        assert!(
            result.is_err(),
            "non-existent cliBinary path must return Err"
        );
    }

    #[test]
    fn test_check_review_model_binary_groq_not_grok_is_noop() {
        // "groq-llama-3" must NOT be classified as Grok (token-equality rule).
        // Probe must succeed even if grok binary is absent.
        assert!(check_review_model_binary(Some("groq-llama-3"), None).is_ok());
    }

    // ---- PrimaryRunnerConfig serde round-trip tests ----

    #[test]
    fn test_primary_runner_absent_is_none() {
        // Missing `primaryRunner` key → `None`.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
    }

    #[test]
    fn test_primary_runner_explicit_null_is_none() {
        // Explicit JSON `null` → `None` (matches FallbackRunnerConfig behavior).
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"primaryRunner": null}"#).unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
    }

    #[test]
    fn test_primary_runner_present_empty_maps() {
        // Object present but no map entries → `Some` with empty maps and defaults.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"primaryRunner": {}}"#).unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert!(pr.claude_fallback_model.is_none());
        assert_eq!(pr.runtime_error_threshold, 2);
        assert!(pr.by_task_type.is_empty());
        assert!(pr.by_id_prefix.is_empty());
        assert!(pr.baseline_tier_routes.is_empty());
    }

    #[test]
    fn test_primary_runner_present_populated() {
        use crate::loop_engine::model::SONNET_MODEL;
        // Fully populated primaryRunner round-trips correctly.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(
                r#"{{
                "primaryRunner": {{
                    "claudeFallbackModel": "{SONNET_MODEL}",
                    "runtimeErrorThreshold": 3,
                    "byTaskType": {{
                        "review":    {{ "provider": "grok", "model": "grok-build" }},
                        "milestone": {{ "provider": "grok", "model": "grok-build" }}
                    }},
                    "byIdPrefix": {{
                        "REVIEW-":    {{ "provider": "grok", "model": "grok-build" }},
                        "MILESTONE-": {{ "provider": "grok", "model": "grok-build" }}
                    }},
                    "baselineTierRoutes": {{
                        "FEAT": {{
                            "high": {{ "provider": "codex", "runtimeErrorFallback": true }},
                            "standard": {{ "provider": "grok", "model": "grok-build" }}
                        }}
                    }}
                }}
            }}"#
            ),
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert_eq!(pr.claude_fallback_model.as_deref(), Some(SONNET_MODEL));
        assert_eq!(pr.runtime_error_threshold, 3);

        let review_spec = pr.by_task_type.get("review").expect("review key missing");
        assert_eq!(review_spec.provider, "grok");
        assert_eq!(review_spec.model, "grok-build");

        let milestone_spec = pr
            .by_task_type
            .get("milestone")
            .expect("milestone key missing");
        assert_eq!(milestone_spec.provider, "grok");
        assert_eq!(milestone_spec.model, "grok-build");

        let rev_prefix_spec = pr.by_id_prefix.get("REVIEW-").expect("REVIEW- key missing");
        assert_eq!(rev_prefix_spec.provider, "grok");
        assert_eq!(rev_prefix_spec.model, "grok-build");

        let ms_prefix_spec = pr
            .by_id_prefix
            .get("MILESTONE-")
            .expect("MILESTONE- key missing");
        assert_eq!(ms_prefix_spec.provider, "grok");
        assert_eq!(ms_prefix_spec.model, "grok-build");

        let feat_tiers = pr
            .baseline_tier_routes
            .get("FEAT")
            .expect("FEAT key missing");
        let high_spec = feat_tiers.get("high").expect("high key missing");
        assert_eq!(high_spec.provider, "codex");
        assert!(high_spec.runtime_error_fallback);
        let standard_spec = feat_tiers.get("standard").expect("standard key missing");
        assert_eq!(standard_spec.provider, "grok");
        assert_eq!(standard_spec.model, "grok-build");
    }

    #[test]
    fn test_primary_runner_accepts_legacy_baseline_tier_routes_alias() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byBaselineTier": {
                        "FEAT": {
                            "opus": { "provider": "codex", "fallbackToClaude": true },
                            "sonnet": { "provider": "grok", "model": "grok-build" }
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("primaryRunner present");
        let feat_tiers = pr
            .baseline_tier_routes
            .get("FEAT")
            .expect("FEAT key missing");
        // read_project_config runs the on-disk migrator as its read normalizer
        // (FIX-002), so legacy tier-key spellings are canonicalized in memory:
        // opus -> high, sonnet -> standard. The legacy keys no longer survive.
        assert!(feat_tiers["high"].runtime_error_fallback);
        assert_eq!(feat_tiers["standard"].model, "grok-build");
    }

    #[test]
    fn test_primary_runner_partial_uses_defaults() {
        // Partial object: only byTaskType set → claudeFallbackModel is None,
        // runtimeErrorThreshold is 2, byIdPrefix is empty.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byTaskType": {
                        "review": { "provider": "grok", "model": "grok-build" }
                    }
                }
            }"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert!(
            pr.claude_fallback_model.is_none(),
            "claudeFallbackModel absent → None"
        );
        assert_eq!(pr.runtime_error_threshold, 2, "default threshold is 2");
        assert!(pr.by_id_prefix.is_empty(), "byIdPrefix absent → empty map");
        assert!(
            pr.baseline_tier_routes.is_empty(),
            "baselineTierRoutes absent → empty map"
        );
        let spec = pr.by_task_type.get("review").expect("review key missing");
        assert_eq!(spec.model, "grok-build");
    }

    #[test]
    fn test_primary_runner_absent_does_not_affect_existing_config() {
        // Ensures that adding `primary_runner` to ProjectConfig doesn't break
        // existing round-trips for other fields when primaryRunner is absent.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version": 1, "additionalAllowedTools": ["Bash(docker:*)"], "mergeFailHaltThreshold": 3}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
        assert_eq!(config.version, 1);
        assert_eq!(config.additional_allowed_tools, vec!["Bash(docker:*)"]);
        assert_eq!(config.merge_fail_halt_threshold, 3);
    }

    #[test]
    fn test_primary_runner_default_impl_is_none() {
        assert!(ProjectConfig::default().primary_runner.is_none());
    }

    // ---- preflight_validate_and_probe tests (FEAT-004) ----

    fn primary_with_one_task_route(
        task_key: &str,
        provider: &str,
        model: &str,
    ) -> PrimaryRunnerConfig {
        let mut by_task_type = HashMap::new();
        by_task_type.insert(
            task_key.to_string(),
            RunnerSpec {
                provider: provider.to_string(),
                model: model.to_string(),
                ..Default::default()
            },
        );
        PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type,
            by_id_prefix: HashMap::new(),
            ..Default::default()
        }
    }

    // ---- RunnerSpec.runtime_error_fallback serde tests (FEAT-005) ----

    #[test]
    fn test_runner_spec_fallback_to_claude_absent_defaults_to_false() {
        // AC: runtimeErrorFallback defaults to false; an absent field deserializes
        // to false. Existing Codex projects keep the legacy auto-block path.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byTaskType": {
                        "spike": { "provider": "codex" }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_task_type
            .remove("spike")
            .expect("spike key present");
        assert!(
            !spec.runtime_error_fallback,
            "absent runtimeErrorFallback must deserialize to false"
        );
    }

    #[test]
    fn test_runner_spec_runtime_error_fallback_true_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byIdPrefix": {
                        "SPIKE-": { "provider": "codex", "runtimeErrorFallback": true }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_id_prefix
            .remove("SPIKE-")
            .expect("SPIKE- key present");
        assert!(
            spec.runtime_error_fallback,
            "runtimeErrorFallback=true must round-trip"
        );
    }

    #[test]
    fn test_runner_spec_runtime_error_fallback_accepts_legacy_alias() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byIdPrefix": {
                        "SPIKE-": { "provider": "codex", "fallbackToClaude": true }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_id_prefix
            .remove("SPIKE-")
            .expect("SPIKE- key present");
        assert!(
            spec.runtime_error_fallback,
            "legacy fallbackToClaude alias must deserialize to runtimeErrorFallback"
        );
    }

    #[test]
    fn test_migrate_runner_spec_map_migrates_every_legacy_spec() {
        // Guards the `unnecessary_fold` allow at migrate_runner_spec_map: the
        // fold must call migrate_runner_spec on EVERY entry. A `.any()`
        // short-circuit would stop after the first spec returning true, leaving
        // the second `fallbackToClaude` un-rewritten. Two legacy specs prove it.
        let mut map = serde_json::json!({
            "FEAT-": { "provider": "codex", "fallbackToClaude": true },
            "FIX-":  { "provider": "codex", "fallbackToClaude": true }
        });
        let changed = migrate_runner_spec_map(Some(&mut map));
        assert!(changed, "migration must report a change");
        for key in ["FEAT-", "FIX-"] {
            let spec = &map[key];
            assert!(
                spec.get("fallbackToClaude").is_none(),
                "{key}: legacy fallbackToClaude must be removed"
            );
            assert_eq!(
                spec["runtimeErrorFallback"],
                serde_json::json!(true),
                "{key}: must be rewritten to runtimeErrorFallback"
            );
        }
    }

    #[test]
    fn test_runner_spec_runtime_error_fallback_snake_case_rejected() {
        // CONTRACT: the field name on the wire is camelCase (runtimeErrorFallback),
        // matching the rest of RunnerSpec's serde rename_all. snake_case must
        // NOT silently set the field.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byTaskType": {
                        "spike": { "provider": "codex", "fallback_to_claude": true }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = read_project_config(dir.path());
        let spec = cfg
            .primary_runner
            .expect("primaryRunner present")
            .by_task_type
            .remove("spike")
            .expect("spike key present");
        assert!(
            !spec.runtime_error_fallback,
            "snake_case key must not set runtime_error_fallback"
        );
    }

    #[test]
    fn test_preflight_passes_pure_claude_config_without_codex_probe() {
        // Acceptance: a pure-Claude config triggers no PATH lookup for codex.
        // We verify by setting CODEX_BINARY to a path that DOES NOT exist —
        // if preflight ever resolved the Codex binary on a pure-Claude config
        // it would fail. The default config has neither a Codex primaryRunner
        // route nor a fallbackRunner, so check_codex_runner_binary must
        // short-circuit on `primary.is_none()` before any path probe runs.
        let prev = std::env::var_os("CODEX_BINARY");
        let bogus = "/tmp/task-mgr-test-nonexistent-codex-binary-feat004";
        unsafe { std::env::set_var("CODEX_BINARY", bogus) };
        let result = preflight_validate_and_probe(&ProjectConfig::default());
        match prev {
            Some(v) => unsafe { std::env::set_var("CODEX_BINARY", v) },
            None => unsafe { std::env::remove_var("CODEX_BINARY") },
        }
        assert!(
            result.is_ok(),
            "pure-Claude config must pass preflight with codex absent from PATH: {result:?}"
        );
    }

    #[test]
    fn test_preflight_codex_route_missing_binary_returns_err() {
        // Acceptance: Codex route + CODEX_BINARY pointing at a nonexistent
        // path returns Err — exactly the failure batch_run must surface
        // BEFORE expanding PRD files.
        let prev = std::env::var_os("CODEX_BINARY");
        let bogus = "/tmp/task-mgr-test-nonexistent-codex-binary-feat004-route";
        unsafe { std::env::set_var("CODEX_BINARY", bogus) };
        let cfg = ProjectConfig {
            primary_runner: Some(primary_with_one_task_route(
                "spike", "codex", "", // codex provider permits blank model
            )),
            ..Default::default()
        };
        let result = preflight_validate_and_probe(&cfg);
        match prev {
            Some(v) => unsafe { std::env::set_var("CODEX_BINARY", v) },
            None => unsafe { std::env::remove_var("CODEX_BINARY") },
        }
        let err = result.expect_err("missing codex binary must return Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("Codex") || msg.contains("codex"),
            "error should mention codex: {msg}"
        );
    }

    #[test]
    fn test_preflight_runs_validation_not_just_probes() {
        // Regression: preflight must run config VALIDATION, not only binary
        // probes. The poison reviewModel⨯Codex combo has no binary to probe,
        // so a probe-only preflight would wave it through — exactly the
        // batch-run parity gap this helper closes.
        let cfg = ProjectConfig {
            review_model: Some("grok-build".to_string()),
            primary_runner: Some(primary_with_one_task_route("review", "codex", "")),
            ..Default::default()
        };
        let err = preflight_validate_and_probe(&cfg).expect_err("preflight must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("reviewModel"),
            "preflight must reject via validation, naming reviewModel: {msg}"
        );
    }

    #[test]
    fn test_preflight_rejects_invalid_fallback_provider() {
        // Acceptance failure-mode: fallbackRunner.provider != "grok" aborts
        // preflight with the same structured error from validate_runner_routing_config.
        let cfg = ProjectConfig {
            fallback_runner: Some(FallbackRunnerConfig {
                enabled: true,
                provider: "codex".to_string(),
                model: "gpt-5-codex".to_string(),
                cli_binary: None,
                runtime_error_threshold: 2,
            }),
            ..Default::default()
        };
        let err = preflight_validate_and_probe(&cfg).expect_err("invalid fallback provider");
        let msg = format!("{err}");
        assert!(
            msg.contains("fallbackRunner.provider") || msg.contains("grok"),
            "error must name fallbackRunner.provider: {msg}"
        );
    }

    // ============ FEAT-006: strict provider parser + provider-only Codex ============

    /// AC (positive): the provider-only Codex rule from V2 is preserved —
    /// `{provider:"codex", model:""}` validates OK. Removing this allowance
    /// would re-introduce a hand-written model-id that the dispatcher's
    /// provider-hint routing now makes unnecessary.
    #[test]
    fn test_validate_accepts_codex_provider_with_blank_model() {
        let mut by_type = HashMap::new();
        by_type.insert(
            "spike".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: "".to_string(),
                ..Default::default()
            },
        );
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_task_type: by_type,
                ..Default::default()
            }),
            ..Default::default()
        };
        validate_runner_routing_config(&cfg).expect("codex provider-only must validate");
    }

    /// AC (negative): a non-Codex route with a blank model is still rejected.
    /// The Codex allowance must be provider-specific — no quiet widening to
    /// other providers.
    #[test]
    fn test_validate_rejects_claude_provider_with_blank_model() {
        let mut by_type = HashMap::new();
        by_type.insert(
            "review".to_string(),
            RunnerSpec {
                provider: "claude".to_string(),
                model: "".to_string(),
                ..Default::default()
            },
        );
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                by_task_type: by_type,
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_runner_routing_config(&cfg).expect_err("blank model must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("model must not be blank") || msg.contains(".model"),
            "error must call out the blank-model rule: {msg}",
        );
    }

    /// Known-bad (the load-bearing AC): an unknown provider typo MUST hard-fail
    /// at validation. With the old `Option`-returning parser, `"groq"` would
    /// silently produce `None`, and the dispatcher would route the task to
    /// Claude (the silent-fallback footgun). The strict parser surfaces it.
    #[test]
    fn test_validate_rejects_unknown_provider_typo() {
        for bad in ["groq", "openai", "codex-cli", "anthropic"] {
            let mut by_id = HashMap::new();
            by_id.insert(
                "TYPO-".to_string(),
                RunnerSpec {
                    provider: bad.to_string(),
                    model: "some-model".to_string(),
                    ..Default::default()
                },
            );
            let cfg = ProjectConfig {
                primary_runner: Some(PrimaryRunnerConfig {
                    by_id_prefix: by_id,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let err = validate_runner_routing_config(&cfg)
                .expect_err(&format!("typo {bad:?} must reject"));
            let msg = format!("{err}");
            assert!(
                msg.contains(bad)
                    && msg.contains("claude")
                    && msg.contains("grok")
                    && msg.contains("codex"),
                "error must name the bad provider {bad:?} AND the allowed set: {msg}",
            );
            // Same value must NOT cause a Codex fallback to fire — verifies
            // the validation hard-fail comes BEFORE any model-based fallback.
            assert!(
                !msg.to_ascii_lowercase().contains("blank"),
                "the typo path must surface the unknown-provider error, not the blank-model rule: {msg}",
            );
        }
    }

    #[test]
    fn test_validate_rejects_unknown_baseline_tier() {
        let mut tiers = HashMap::new();
        tiers.insert(
            "superopus".to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                ..Default::default()
            },
        );
        let mut baseline_tier_routes = HashMap::new();
        baseline_tier_routes.insert("FEAT".to_string(), tiers);
        let cfg = ProjectConfig {
            primary_runner: Some(PrimaryRunnerConfig {
                baseline_tier_routes,
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_runner_routing_config(&cfg).expect_err("unknown tier must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("baselineTierRoutes.FEAT.superopus") && msg.contains("low"),
            "error must name the bad tier and allowed tiers: {msg}"
        );
    }

    /// CONTRACT: `EffectiveRunnerInput` field names match the struct in
    /// `engine.rs` exactly — `model` and `provider_hint`. A rename in
    /// `engine.rs` without a matching rename here (or downstream) would
    /// break the production drift guard. Grep the engine source rather
    /// than re-importing so this test cannot be silently weakened by an
    /// `impl Into` shim.
    #[test]
    fn test_effective_runner_input_field_names_in_engine_rs() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/loop_engine/engine.rs"
        ))
        .expect("engine.rs must be readable for the CONTRACT check");
        assert!(
            src.contains("pub struct EffectiveRunnerInput"),
            "engine.rs must define `pub struct EffectiveRunnerInput`",
        );
        assert!(
            src.contains("pub model: Option<&'a str>"),
            "EffectiveRunnerInput::model field name/type must be `pub model: Option<&'a str>`",
        );
        assert!(
            src.contains("pub provider_hint: Option<model::Provider>"),
            "EffectiveRunnerInput::provider_hint field name/type must be \
             `pub provider_hint: Option<model::Provider>`",
        );
    }

    // ---- write_review_model tests ----

    #[test]
    fn test_write_review_model_sets_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();
        write_review_model(dir.path(), Some("grok-build")).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains("\"reviewModel\""), "key should be set");
        assert!(raw.contains("grok-build"), "value should be present");
        // load-bearing: unrelated key must survive
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(
            raw.contains("Bash(docker:*)"),
            "additionalAllowedTools value lost"
        );
    }

    #[test]
    fn test_write_review_model_removes_key_when_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"reviewModel":"grok-build"}"#,
        )
        .unwrap();
        write_review_model(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(!raw.contains("reviewModel"), "key should be removed: {raw}");
    }

    #[test]
    fn test_write_review_model_none_on_absent_key_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        write_review_model(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("reviewModel"),
            "key should stay absent: {raw}"
        );
    }

    #[test]
    fn test_write_review_model_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        write_review_model(dir.path(), Some("grok-build")).unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.review_model.as_deref(), Some("grok-build"));
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("\"version\""),
            "version key must be present on new file"
        );
    }

    #[test]
    fn test_write_review_model_preserves_all_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"embeddingModel":"x","ollamaUrl":"http://x","rerankerModel":"m"}"#,
        )
        .unwrap();
        write_review_model(dir.path(), Some("grok-build")).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(
            raw.contains("Bash(docker:*)"),
            "additionalAllowedTools value lost"
        );
        assert!(raw.contains("embeddingModel"), "embeddingModel lost");
        assert!(raw.contains("ollamaUrl"), "ollamaUrl lost");
        assert!(raw.contains("rerankerModel"), "rerankerModel lost");
        assert!(raw.contains("grok-build"), "reviewModel not set");
    }

    #[test]
    fn test_write_review_model_malformed_json_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "not json at all").unwrap();
        let result = write_review_model(dir.path(), Some("grok-build"));
        assert!(result.is_err(), "malformed JSON must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("config.json"),
            "error must name the file: {msg}"
        );
    }

    // ---- write_fallback_runner tests ----

    #[test]
    fn test_write_fallback_runner_sets_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();
        let cfg = FallbackRunnerConfig {
            enabled: true,
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            cli_binary: None,
            runtime_error_threshold: 2,
        };
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains("\"fallbackRunner\""), "key should be set");
        assert!(raw.contains("\"enabled\""), "enabled field must be present");
        // load-bearing: unrelated key must survive
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(
            raw.contains("Bash(docker:*)"),
            "additionalAllowedTools value lost"
        );
        let config = read_project_config(dir.path());
        let fr = config
            .fallback_runner
            .expect("fallbackRunner should be Some");
        assert!(fr.enabled);
        assert_eq!(fr.provider, "grok");
        assert_eq!(fr.model, "grok-build");
        assert!(fr.cli_binary.is_none());
    }

    #[test]
    fn test_write_fallback_runner_with_cli_binary() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FallbackRunnerConfig {
            enabled: true,
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            cli_binary: Some("/usr/local/bin/grok".to_string()),
            runtime_error_threshold: 3,
        };
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let config = read_project_config(dir.path());
        let fr = config
            .fallback_runner
            .expect("fallbackRunner should be Some");
        assert_eq!(fr.cli_binary.as_deref(), Some("/usr/local/bin/grok"));
        assert_eq!(fr.runtime_error_threshold, 3);
    }

    #[test]
    fn test_write_fallback_runner_removes_key_when_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"fallbackRunner":{"enabled":true,"provider":"grok","model":"grok-build","runtimeErrorThreshold":2}}"#,
        )
        .unwrap();
        write_fallback_runner(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("fallbackRunner"),
            "key should be removed: {raw}"
        );
    }

    #[test]
    fn test_write_fallback_runner_none_on_absent_key_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        write_fallback_runner(dir.path(), None).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("fallbackRunner"),
            "key should stay absent: {raw}"
        );
    }

    #[test]
    fn test_write_fallback_runner_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FallbackRunnerConfig::default();
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let config = read_project_config(dir.path());
        assert!(
            config.fallback_runner.is_some(),
            "fallbackRunner should be set"
        );
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            raw.contains("\"version\""),
            "version key must be present on new file"
        );
    }

    #[test]
    fn test_write_fallback_runner_malformed_json_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{{bad json").unwrap();
        let result = write_fallback_runner(dir.path(), Some(&FallbackRunnerConfig::default()));
        assert!(result.is_err(), "malformed JSON must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("config.json"),
            "error must name the file: {msg}"
        );
    }

    #[test]
    fn test_write_fallback_runner_cli_binary_none_not_serialized_as_null() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FallbackRunnerConfig {
            enabled: false,
            provider: "grok".to_string(),
            model: "grok-build".to_string(),
            cli_binary: None,
            runtime_error_threshold: 2,
        };
        write_fallback_runner(dir.path(), Some(&cfg)).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("\"cliBinary\""),
            "None cli_binary should be omitted, not null"
        );
    }

    // ============ TEST-INIT-002: config migration surface ============
    // Covers (1) read-path serde-alias equivalence, (2) the on-disk
    // `update_project_config_format` rewrite — key-preservation, idempotency,
    // and malformed/non-object handling — and (3) merge-collision precedence.
    //
    // Learnings [4393]/[4396]/[4378]/[4561]: the on-disk migration MUST
    // round-trip through `serde_json::Value` and mutate keys in place — NEVER
    // reserialize a typed struct, which would silently drop unknown fields.
    // Real config.json fixtures on disk (tempdir) are used so the atomic-write
    // path (tempfile + persist) is actually exercised.

    /// AC1: a legacy-key config (`byBaselineTier` + `fallbackToClaude`)
    /// deserializes into a `ProjectConfig` structurally EQUAL to the same
    /// config written with the canonical top-level keys (`baselineTierRoutes`
    /// + `runtimeErrorFallback`).
    ///
    /// Post FIX-002 the mechanism making this hold is the in-memory MIGRATOR,
    /// not the serde `alias` attributes: `read_project_config` runs
    /// `migrate_project_config_value` as its read normalizer and deserializes
    /// from the MIGRATED `Value`. The migrator both renames the legacy keys
    /// AND canonicalizes the HashMap tier keys (`opus` -> `high`,
    /// `sonnet` -> `standard`). Both inputs therefore normalize to the same
    /// canonical tier keys in memory, so equality no longer depends on the two
    /// fixtures spelling the tier keys identically — it is true canonical
    /// equivalence (the sanity assertions below index the canonical `high` key).
    #[test]
    fn test_read_legacy_keys_equal_canonical_equivalent() {
        let legacy_dir = tempfile::tempdir().unwrap();
        fs::write(
            legacy_dir.path().join("config.json"),
            r#"{
                "version": 1,
                "additionalAllowedTools": ["Bash(docker:*)"],
                "primaryRunner": {
                    "byBaselineTier": {
                        "FEAT": {
                            "opus": { "provider": "codex", "fallbackToClaude": true },
                            "sonnet": { "provider": "grok", "model": "grok-build" }
                        }
                    },
                    "byIdPrefix": {
                        "SPIKE-": { "provider": "codex", "fallbackToClaude": true }
                    }
                }
            }"#,
        )
        .unwrap();

        let canonical_dir = tempfile::tempdir().unwrap();
        fs::write(
            canonical_dir.path().join("config.json"),
            r#"{
                "version": 1,
                "additionalAllowedTools": ["Bash(docker:*)"],
                "primaryRunner": {
                    "baselineTierRoutes": {
                        "FEAT": {
                            "opus": { "provider": "codex", "runtimeErrorFallback": true },
                            "sonnet": { "provider": "grok", "model": "grok-build" }
                        }
                    },
                    "byIdPrefix": {
                        "SPIKE-": { "provider": "codex", "runtimeErrorFallback": true }
                    }
                }
            }"#,
        )
        .unwrap();

        let legacy = read_project_config(legacy_dir.path());
        let canonical = read_project_config(canonical_dir.path());
        assert_eq!(
            legacy, canonical,
            "legacy byBaselineTier/fallbackToClaude must deserialize identically to canonical keys"
        );

        // Sanity: the routes actually populated — guards against a vacuous
        // pass where the alias silently dropped and both sides became empty
        // (the index below would also panic in that case).
        let pr = legacy.primary_runner.expect("primaryRunner present");
        // The read normalizer (FIX-002) canonicalizes tier keys: opus -> high.
        assert!(
            pr.baseline_tier_routes["FEAT"]["high"].runtime_error_fallback,
            "byBaselineTier→baseline_tier_routes alias must populate the tier route"
        );
        assert!(
            pr.by_id_prefix["SPIKE-"].runtime_error_fallback,
            "fallbackToClaude→runtime_error_fallback alias must populate the spec field"
        );
    }

    /// AC2 + AC6 (known-bad discriminator): `update_project_config_format`
    /// canonicalizes legacy routing keys AND preserves unrelated keys
    /// value-for-value. The top-level `customField` and the in-spec
    /// `customSpecField` (neither is a field on `ProjectConfig`/`RunnerSpec`)
    /// are the discriminators: if the migration rebuilt the JSON from a typed
    /// struct instead of mutating the `serde_json::Value` tree in place, both
    /// unknown fields would silently vanish and these assertions would fail.
    #[test]
    fn test_update_format_canonicalizes_and_preserves_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        let input = r#"{
            "version": 1,
            "additionalAllowedTools": ["Bash(docker:*)"],
            "embeddingModel": "custom-embed",
            "customField": { "nested": [1, 2, 3] },
            "primaryRunner": {
                "byBaselineTier": {
                    "FEAT": {
                        "opus": { "provider": "codex", "fallbackToClaude": true }
                    }
                },
                "byIdPrefix": {
                    "SPIKE-": { "provider": "codex", "fallbackToClaude": true, "customSpecField": 42 }
                }
            }
        }"#;
        let original: serde_json::Value = serde_json::from_str(input).unwrap();
        fs::write(dir.path().join("config.json"), input).unwrap();

        let changed = update_project_config_format(dir.path()).unwrap();
        assert!(changed, "legacy keys must trigger a rewrite");

        let migrated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("config.json")).unwrap())
                .unwrap();

        // Legacy top-level container removed; canonical present.
        let primary = &migrated["primaryRunner"];
        assert!(
            primary.get("byBaselineTier").is_none(),
            "byBaselineTier must be removed after canonicalization"
        );
        // Tier key canonicalized opus -> high; spec field canonicalized.
        let feat = &primary["baselineTierRoutes"]["FEAT"];
        assert!(
            feat.get("opus").is_none(),
            "opus tier key must canonicalize to high"
        );
        assert_eq!(
            feat["high"]["runtimeErrorFallback"],
            serde_json::json!(true),
            "fallbackToClaude must canonicalize to runtimeErrorFallback in the tier spec"
        );
        assert!(
            feat["high"].get("fallbackToClaude").is_none(),
            "legacy fallbackToClaude must be removed from the tier spec"
        );
        // byIdPrefix spec canonicalized; unknown spec field preserved.
        let spike = &primary["byIdPrefix"]["SPIKE-"];
        assert_eq!(spike["runtimeErrorFallback"], serde_json::json!(true));
        assert!(spike.get("fallbackToClaude").is_none());
        assert_eq!(
            spike["customSpecField"],
            serde_json::json!(42),
            "KNOWN-BAD DISCRIMINATOR: unknown spec field must survive in-place Value mutation"
        );

        // Unrelated top-level keys survive value-for-value. `customField` is
        // the load-bearing discriminator — a typed-struct rebuild would drop
        // it because it is not a field on ProjectConfig.
        for key in [
            "version",
            "additionalAllowedTools",
            "embeddingModel",
            "customField",
        ] {
            assert_eq!(
                migrated[key], original[key],
                "unrelated key {key} must survive the rewrite unchanged"
            );
        }
    }

    /// AC3: a second `update_project_config_format` run returns `Ok(false)` and
    /// does not modify the file.
    #[test]
    fn test_update_format_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byBaselineTier": {
                        "FEAT": { "opus": { "provider": "codex", "fallbackToClaude": true } }
                    }
                }
            }"#,
        )
        .unwrap();

        let first = update_project_config_format(dir.path()).unwrap();
        assert!(first, "first run must rewrite legacy keys");
        let after_first = fs::read_to_string(dir.path().join("config.json")).unwrap();

        let second = update_project_config_format(dir.path()).unwrap();
        assert!(!second, "second run must be a no-op returning Ok(false)");
        let after_second = fs::read_to_string(dir.path().join("config.json")).unwrap();

        assert_eq!(
            after_first, after_second,
            "an idempotent run must leave the file byte-identical"
        );
    }

    /// AC4: malformed JSON returns `Err` and leaves the file untouched.
    #[test]
    fn test_update_format_malformed_json_errors_and_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let original = "not json at all {{{";
        fs::write(dir.path().join("config.json"), original).unwrap();

        let err =
            update_project_config_format(dir.path()).expect_err("malformed JSON must return Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("config.json") && msg.contains("malformed"),
            "error must name the file and explain the failure: {msg}"
        );

        let after = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert_eq!(after, original, "malformed file must be left untouched");
    }

    /// AC4: a syntactically-valid but non-object JSON document (e.g. an array)
    /// returns `Err` and leaves the file untouched.
    #[test]
    fn test_update_format_non_object_json_errors_and_preserves_file() {
        let dir = tempfile::tempdir().unwrap();
        let original = "[1, 2, 3]";
        fs::write(dir.path().join("config.json"), original).unwrap();

        let err =
            update_project_config_format(dir.path()).expect_err("non-object JSON must return Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("not a JSON object"),
            "error must explain the non-object shape: {msg}"
        );

        let after = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert_eq!(after, original, "non-object file must be left untouched");
    }

    /// AC5: when BOTH `byBaselineTier` (legacy `opus`) and `baselineTierRoutes`
    /// (canonical `high`) exist for the same prefix, the canonical route wins
    /// and the legacy entry is dropped. `merge_baseline_tier_routes` uses
    /// `.or_insert`, so a pre-existing canonical value is never overwritten.
    #[test]
    fn test_update_format_collision_canonical_wins() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "byBaselineTier": {
                        "FEAT": { "opus": { "provider": "grok", "model": "legacy-loser" } }
                    },
                    "baselineTierRoutes": {
                        "FEAT": { "high": { "provider": "codex", "runtimeErrorFallback": true } }
                    }
                }
            }"#,
        )
        .unwrap();

        let changed = update_project_config_format(dir.path()).unwrap();
        assert!(changed, "presence of byBaselineTier must trigger a rewrite");

        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        let migrated: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let feat = &migrated["primaryRunner"]["baselineTierRoutes"]["FEAT"];

        // Canonical `high` route preserved verbatim; legacy `opus` value dropped.
        assert_eq!(
            feat["high"]["provider"],
            serde_json::json!("codex"),
            "canonical high route must win the collision"
        );
        assert!(
            feat.get("opus").is_none(),
            "legacy opus key must be dropped, not retained alongside high"
        );
        assert!(
            migrated["primaryRunner"].get("byBaselineTier").is_none(),
            "legacy byBaselineTier container must be removed"
        );
        assert!(
            !raw.contains("legacy-loser"),
            "the dropped legacy spec value must not survive anywhere in the file"
        );
    }

    /// FIX-003: the rewrite must be permission-neutral. The atomic tempfile is
    /// created 0o600; without re-applying the original mode, a group/world
    /// readable config (0o644) would be silently narrowed by `persist`.
    #[cfg(unix)]
    #[test]
    fn test_update_format_preserves_original_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
                "primaryRunner": {
                    "byBaselineTier": {
                        "FEAT": { "opus": { "provider": "codex" } }
                    }
                }
            }"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let changed = update_project_config_format(dir.path()).unwrap();
        assert!(changed, "legacy key must trigger a rewrite");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "rewrite must preserve the original 0o644 mode, not narrow to the 0o600 tempfile default"
        );
    }
}
