use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::error::{TaskMgrError, TaskMgrResult};

/// Configuration for the Grok fallback runner (US-005, FR-006).
///
/// When `enabled = true`, the loop engine promotes tasks to the Grok CLI
/// after the Claude overflow ladder is exhausted (rung 4) or after
/// `runtime_error_threshold` consecutive `RuntimeError` rounds. Absent or
/// `enabled = false` → no change to the existing 4-rung ladder.
#[derive(Debug, Clone, Deserialize)]
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
    #[serde(default)]
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

/// A provider + model pair used as a routing target in `PrimaryRunnerConfig`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunnerSpec {
    /// Provider name (e.g. `"grok"`, `"claude"`).
    pub provider: String,
    /// Model identifier passed to the provider CLI (e.g. `"grok-4"`).
    pub model: String,
}

/// Per-task-type and per-id-prefix routing for the primary runner.
///
/// Routes specific task types (e.g. `"review"`, `"milestone"`) or ID prefixes
/// (e.g. `"REVIEW-"`, `"MILESTONE-"`) to a non-default runner, while all other
/// tasks continue to use the default Claude model/runner.
///
/// Phase 1: schema + serde only — resolution-chain wiring comes in a later phase.
#[derive(Debug, Clone, Deserialize)]
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
}

impl Default for PrimaryRunnerConfig {
    fn default() -> Self {
        Self {
            claude_fallback_model: None,
            runtime_error_threshold: default_primary_runtime_error_threshold(),
            by_task_type: HashMap::new(),
            by_id_prefix: HashMap::new(),
        }
    }
}

/// Per-project loop configuration read from `.task-mgr/config.json`.
///
/// Allows projects to extend the default tool allowlist with project-specific
/// tools (e.g., `docker`, `curl`, `./scripts/*`) without modifying the core
/// default. Forward-compatible: unknown fields are silently ignored.
#[derive(Debug, Deserialize)]
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
    /// model id (e.g. `"grok-4"`). Absent key or explicit `null` → `None`.
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

/// Read project config from `<db_dir>/config.json`.
///
/// Returns default (empty) config if the file doesn't exist.
/// Warns on invalid JSON but does not fail — returns defaults instead.
pub fn read_project_config(db_dir: &Path) -> ProjectConfig {
    let path = db_dir.join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
            eprintln!("\x1b[33m[warn]\x1b[0m Invalid .task-mgr/config.json: {e}");
            ProjectConfig::default()
        }),
        Err(_) => ProjectConfig::default(),
    }
}

/// Set (or clear) the `defaultModel` field in `<db_dir>/config.json` without
/// clobbering other fields — including unknown forward-compat ones.
///
/// Pass `Some(model)` to set, `None` to remove the key entirely.
/// Creates the file and parent dir if needed. Writes atomically via a
/// same-directory tempfile + rename so readers never see a half-written JSON.
pub fn write_default_model(db_dir: &Path, model: Option<&str>) -> std::io::Result<()> {
    use std::io::Write;

    let path = db_dir.join("config.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut value: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({ "version": 1 }))
        }
        _ => serde_json::json!({ "version": 1 }),
    };
    let obj = value.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config.json is not a JSON object",
        )
    })?;
    match model {
        Some(m) => {
            obj.insert(
                "defaultModel".to_string(),
                serde_json::Value::String(m.to_string()),
            );
        }
        None => {
            obj.remove("defaultModel");
        }
    }

    let contents = serde_json::to_string_pretty(&value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".json")
        .tempfile_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.write_all(b"\n")?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(())
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
            r#"{"reviewModel": "grok-4"}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert_eq!(config.review_model.as_deref(), Some("grok-4"));
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
            r#"{"review_model": "grok-4"}"#,
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
        let result = check_review_model_binary(Some("grok-4"), None);
        unsafe { std::env::remove_var("GROK_BINARY") };
        assert!(result.is_err(), "missing grok binary must return Err");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("grok-4"),
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
        let result = check_review_model_binary(Some("grok-4-fast"), Some(bogus_cli));
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
        fs::write(
            dir.path().join("config.json"),
            r#"{"primaryRunner": null}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        assert!(config.primary_runner.is_none());
    }

    #[test]
    fn test_primary_runner_present_empty_maps() {
        // Object present but no map entries → `Some` with empty maps and defaults.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"primaryRunner": {}}"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert!(pr.claude_fallback_model.is_none());
        assert_eq!(pr.runtime_error_threshold, 2);
        assert!(pr.by_task_type.is_empty());
        assert!(pr.by_id_prefix.is_empty());
    }

    #[test]
    fn test_primary_runner_present_populated() {
        // Fully populated primaryRunner round-trips correctly.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "primaryRunner": {
                    "claudeFallbackModel": "claude-sonnet-4-6",
                    "runtimeErrorThreshold": 3,
                    "byTaskType": {
                        "review":    { "provider": "grok", "model": "grok-4" },
                        "milestone": { "provider": "grok", "model": "grok-4" }
                    },
                    "byIdPrefix": {
                        "REVIEW-":    { "provider": "grok", "model": "grok-4" },
                        "MILESTONE-": { "provider": "grok", "model": "grok-4" }
                    }
                }
            }"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert_eq!(pr.claude_fallback_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(pr.runtime_error_threshold, 3);

        let review_spec = pr.by_task_type.get("review").expect("review key missing");
        assert_eq!(review_spec.provider, "grok");
        assert_eq!(review_spec.model, "grok-4");

        let milestone_spec = pr.by_task_type.get("milestone").expect("milestone key missing");
        assert_eq!(milestone_spec.provider, "grok");
        assert_eq!(milestone_spec.model, "grok-4");

        let rev_prefix_spec = pr.by_id_prefix.get("REVIEW-").expect("REVIEW- key missing");
        assert_eq!(rev_prefix_spec.provider, "grok");
        assert_eq!(rev_prefix_spec.model, "grok-4");

        let ms_prefix_spec = pr.by_id_prefix.get("MILESTONE-").expect("MILESTONE- key missing");
        assert_eq!(ms_prefix_spec.provider, "grok");
        assert_eq!(ms_prefix_spec.model, "grok-4");
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
                        "review": { "provider": "grok", "model": "grok-4-fast" }
                    }
                }
            }"#,
        )
        .unwrap();
        let config = read_project_config(dir.path());
        let pr = config.primary_runner.expect("should be Some");
        assert!(pr.claude_fallback_model.is_none(), "claudeFallbackModel absent → None");
        assert_eq!(pr.runtime_error_threshold, 2, "default threshold is 2");
        assert!(pr.by_id_prefix.is_empty(), "byIdPrefix absent → empty map");
        let spec = pr.by_task_type.get("review").expect("review key missing");
        assert_eq!(spec.model, "grok-4-fast");
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
}
