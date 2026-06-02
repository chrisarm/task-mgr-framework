//! Command handlers for `task-mgr models <list|set-default|unset-default|show>`.

use std::io;
use std::path::Path;

use super::api::{ApiError, check_opt_in, fetch_models, sort_newest_first};
use super::cache;
use super::ensure_default::{fallback_choices, prompt_with_choices};
use super::picker::ModelChoice;
use crate::loop_engine::model::{
    EFFORT_FOR_DIFFICULTY, HAIKU_MODEL, OPUS_MODEL, Provider, SONNET_MODEL, parse_config_provider,
    provider_for_model,
};
use crate::loop_engine::project_config::{
    FallbackRunnerConfig, check_fallback_runner_binary, check_review_model_binary,
    read_project_config, write_default_model as write_project_default,
    write_fallback_runner as write_project_fallback_runner,
    write_review_model as write_project_review_model,
};
use crate::loop_engine::user_config::{
    read_user_config, write_default_model as write_user_default,
    write_fallback_runner as write_user_fallback_runner,
    write_review_model as write_user_review_model,
};
use crate::output::ui;

/// Options for `models list`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListOpts {
    /// Consult the Anthropic API if possible (requires opt-in).
    pub remote: bool,
    /// Force cache refresh before fetching. Implies `remote`.
    pub refresh: bool,
}

/// Options for `models set-default`.
#[derive(Debug, Clone)]
pub struct SetDefaultOpts {
    /// Model id to pin. When `None`, prompts interactively.
    pub model: Option<String>,
    /// Write to project config instead of user config.
    pub project: bool,
}

/// Options for `models unset-default`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsetDefaultOpts {
    /// Remove from project config instead of user config.
    pub project: bool,
}

/// Source labels reported by `models show`. Kept as an enum so tests can
/// assert exact strings.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DefaultSource {
    Project,
    User,
    None,
}

impl DefaultSource {
    pub fn label(&self) -> &'static str {
        match self {
            DefaultSource::Project => "project",
            DefaultSource::User => "user",
            DefaultSource::None => "none",
        }
    }
}

/// `task-mgr models list` entry point.
pub fn handle_list(db_dir: &Path, opts: ListOpts) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    handle_list_to(&mut out, db_dir, opts)
}

/// Testable variant that writes to an arbitrary `Write`.
pub fn handle_list_to<W: io::Write>(
    writer: &mut W,
    _db_dir: &Path,
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

    writeln!(writer, "Models (built-in list from loop_engine::model):")?;
    writeln!(writer)?;
    writeln!(writer, "  Opus   → {OPUS_MODEL}")?;
    writeln!(writer, "  Sonnet → {SONNET_MODEL}")?;
    writeln!(writer, "  Haiku  → {HAIKU_MODEL}")?;
    writeln!(writer)?;
    writeln!(writer, "Difficulty → --effort mapping:")?;
    for (difficulty, effort) in EFFORT_FOR_DIFFICULTY {
        writeln!(writer, "  {:<7} → {}", difficulty, effort)?;
    }
    if !want_remote {
        writeln!(writer)?;
        writeln!(
            writer,
            "(run with --remote to fetch the live list; requires ANTHROPIC_API_KEY + TASK_MGR_USE_API=1)"
        )?;
    }
    Ok(())
}

/// `task-mgr models set-default` entry point.
pub fn handle_set_default(db_dir: &Path, opts: SetDefaultOpts) -> io::Result<()> {
    let pick = match opts.model {
        Some(m) => Some(m),
        None => {
            let choices = choice_list_for_prompt();
            let stdin = io::stdin();
            prompt_with_choices(stdin.lock(), io::stderr().lock(), &choices)?
        }
    };
    let Some(id) = pick else {
        ui::emit_data("(no default set)");
        return Ok(());
    };
    if opts.project {
        write_project_default(db_dir, Some(&id))?;
        ui::emit_data(&format!("Set project default model to {id}"));
    } else {
        write_user_default(Some(&id))?;
        ui::emit_data(&format!("Set user default model to {id}"));
    }
    Ok(())
}

/// `task-mgr models unset-default` entry point.
pub fn handle_unset_default(db_dir: &Path, opts: UnsetDefaultOpts) -> io::Result<()> {
    if opts.project {
        write_project_default(db_dir, None)?;
        ui::emit_data("Cleared project default model");
    } else {
        write_user_default(None)?;
        ui::emit_data("Cleared user default model");
    }
    Ok(())
}

/// `task-mgr models show` entry point.
///
/// `db_dir_source` reports how `db_dir` was resolved (CLI flag, env var,
/// worktree-anchored default, or plain cwd-default). Surfacing it here is
/// cheap and pays for itself the next time someone wonders why their
/// `task-mgr` invocation in a worktree shell is reading a different DB
/// than they expected.
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
    let project_cfg = read_project_config(db_dir);
    let user_cfg = read_user_config();
    render_default_header(writer, db_dir, db_dir_source, &project_cfg, &user_cfg)?;
    writeln!(writer)?;
    writeln!(writer, "Routing:")?;
    render_review_model(writer, &project_cfg)?;
    render_fallback_runner(writer, &project_cfg)?;
    render_primary_runner(writer, &project_cfg)
}

fn render_default_header<W: io::Write>(
    writer: &mut W,
    db_dir: &Path,
    db_dir_source: crate::db::DbDirSource,
    project_cfg: &crate::loop_engine::project_config::ProjectConfig,
    user_cfg: &crate::loop_engine::user_config::UserConfig,
) -> io::Result<()> {
    let (model, source) = match (
        project_cfg.default_model.as_ref(),
        user_cfg.default_model.as_ref(),
    ) {
        (Some(p), _) => (Some(p.clone()), DefaultSource::Project),
        (None, Some(u)) => (Some(u.clone()), DefaultSource::User),
        (None, None) => (None, DefaultSource::None),
    };
    match model {
        Some(m) => writeln!(writer, "Default model: {m}  (source: {})", source.label())?,
        None => writeln!(
            writer,
            "No default model set. Pick one with `task-mgr models set-default`, \
             or rely on per-PRD / per-task overrides."
        )?,
    }
    writeln!(
        writer,
        "db_dir: {}  (source: {})",
        db_dir.display(),
        db_dir_source.label()
    )
}

fn render_review_model<W: io::Write>(
    writer: &mut W,
    project_cfg: &crate::loop_engine::project_config::ProjectConfig,
) -> io::Result<()> {
    match &project_cfg.review_model {
        Some(m) => {
            let pname = provider_label(provider_for_model(Some(m)));
            writeln!(writer, "  reviewModel:    {m} (provider: {pname})")
        }
        None => writeln!(writer, "  reviewModel:    (unset)"),
    }
}

fn render_fallback_runner<W: io::Write>(
    writer: &mut W,
    project_cfg: &crate::loop_engine::project_config::ProjectConfig,
) -> io::Result<()> {
    match &project_cfg.fallback_runner {
        Some(fr) if fr.enabled => writeln!(
            writer,
            "  fallbackRunner: enabled, provider={}, model={}, threshold={}",
            fr.provider, fr.model, fr.runtime_error_threshold
        ),
        _ => writeln!(writer, "  fallbackRunner: disabled"),
    }
}

fn render_primary_runner<W: io::Write>(
    writer: &mut W,
    project_cfg: &crate::loop_engine::project_config::ProjectConfig,
) -> io::Result<()> {
    let Some(pr) = &project_cfg.primary_runner else {
        return writeln!(writer, "  primaryRunner:  (no routes)");
    };
    if pr.by_task_type.is_empty() && pr.by_id_prefix.is_empty() && pr.by_baseline_tier.is_empty() {
        return writeln!(writer, "  primaryRunner:  (no routes)");
    }
    writeln!(writer, "  primaryRunner:")?;
    let mut by_task_type: Vec<_> = pr.by_task_type.iter().collect();
    by_task_type.sort_by_key(|(k, _)| k.as_str());
    for (task_type, spec) in &by_task_type {
        let pname = runner_spec_provider_label(spec);
        writeln!(
            writer,
            "    byTaskType[{task_type}] -> {pname}/{}",
            spec.model
        )?;
    }
    let mut by_id_prefix: Vec<_> = pr.by_id_prefix.iter().collect();
    by_id_prefix.sort_by_key(|(k, _)| k.as_str());
    for (prefix, spec) in &by_id_prefix {
        let pname = runner_spec_provider_label(spec);
        writeln!(writer, "    byIdPrefix[{prefix}] -> {pname}/{}", spec.model)?;
    }
    let mut by_baseline_tier: Vec<_> = pr.by_baseline_tier.iter().collect();
    by_baseline_tier.sort_by_key(|(prefix, _)| prefix.as_str());
    for (prefix, tiers) in &by_baseline_tier {
        let mut tier_entries: Vec<_> = tiers.iter().collect();
        tier_entries.sort_by_key(|(tier, _)| tier.as_str());
        for (tier, spec) in tier_entries {
            let pname = runner_spec_provider_label(spec);
            writeln!(
                writer,
                "    byBaselineTier[{prefix}][{tier}] -> {pname}/{}",
                spec.model
            )?;
        }
    }
    Ok(())
}

fn runner_spec_provider_label(spec: &crate::loop_engine::project_config::RunnerSpec) -> &str {
    parse_config_provider(&spec.provider)
        .map(provider_label)
        .unwrap_or(spec.provider.as_str())
}

/// Human-readable display name for a [`Provider`].
fn provider_label(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Claude",
        Provider::Grok => "Grok",
        Provider::Codex => "Codex",
    }
}

/// Build the choice list used by the prompt: live if opt-in and fetch
/// succeeds, else the hardcoded fallback.
fn choice_list_for_prompt() -> Vec<ModelChoice> {
    match fetch_live_list() {
        Ok(remote) => remote
            .into_iter()
            .map(|m| {
                let tier = classify_tier(&m.id);
                let note = m.created_at.map(|dt| dt.format("%Y-%m-%d").to_string());
                ModelChoice {
                    id: m.id,
                    tier,
                    note,
                }
            })
            .collect(),
        Err(_) => fallback_choices(),
    }
}

fn classify_tier(id: &str) -> String {
    let lower = id.to_lowercase();
    if lower.contains("opus") {
        "Opus".into()
    } else if lower.contains("sonnet") {
        "Sonnet".into()
    } else if lower.contains("haiku") {
        "Haiku".into()
    } else {
        String::new()
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

/// Options for `models set-fallback`.
#[derive(Debug, Clone)]
pub struct SetFallbackOpts {
    /// Enable the fallback runner.
    pub enable: bool,
    /// Disable the fallback runner.
    pub disable: bool,
    /// Provider name (only "grok" is valid in v1).
    pub provider: Option<String>,
    /// Model id.
    pub model: Option<String>,
    /// Absolute path to the Grok CLI binary.
    pub cli_binary: Option<String>,
    /// Consecutive RuntimeError rounds before the fallback fires.
    pub runtime_error_threshold: Option<u32>,
    /// Write to project config instead of user config.
    pub project: bool,
}

/// `task-mgr models set-review-model` entry point.
///
/// Probes the Grok binary **before** writing when the model routes to Grok.
/// Warns when a `primaryRunner` Codex route already exists (the engine
/// rejects that combination at loop startup).
pub fn handle_set_review_model(db_dir: &Path, model: &str, project: bool) -> io::Result<()> {
    let project_cfg = read_project_config(db_dir);

    // Probe binary BEFORE write if the model routes to Grok.
    // Use the existing fallbackRunner.cliBinary as the path hint (same as
    // the loop startup probe — CONTRACT: same resolver function).
    let cli_binary = project_cfg
        .fallback_runner
        .as_ref()
        .and_then(|fr| fr.cli_binary.clone());
    check_review_model_binary(Some(model), cli_binary.as_deref())
        .map_err(|e| io::Error::other(e.to_string()))?;

    // Warn when primaryRunner has Codex routes (engine rejects this combo).
    let has_codex_primary = project_cfg
        .primary_runner
        .as_ref()
        .map(|pr| {
            pr.by_task_type.values().any(|s| s.provider == "codex")
                || pr.by_id_prefix.values().any(|s| s.provider == "codex")
                || pr
                    .by_baseline_tier
                    .values()
                    .any(|tiers| tiers.values().any(|s| s.provider == "codex"))
        })
        .unwrap_or(false);
    if has_codex_primary {
        ui::emit_err(
            "[warn] primaryRunner has Codex routes — the engine rejects reviewModel + Codex \
             at loop startup; remove the Codex routes or leave reviewModel unset",
        );
    }

    if project {
        write_project_review_model(db_dir, Some(model))?;
        ui::emit_data(&format!("Set project reviewModel to {model}"));
    } else {
        write_user_review_model(Some(model))?;
        ui::emit_data(&format!("Set user reviewModel to {model}"));
    }
    Ok(())
}

/// `task-mgr models unset-review-model` entry point.
pub fn handle_unset_review_model(db_dir: &Path, project: bool) -> io::Result<()> {
    if project {
        write_project_review_model(db_dir, None)?;
        ui::emit_data("Cleared project reviewModel");
    } else {
        write_user_review_model(None)?;
        ui::emit_data("Cleared user reviewModel");
    }
    Ok(())
}

/// `task-mgr models set-fallback` entry point.
///
/// Builds a fresh `FallbackRunnerConfig` from the provided options (no merge
/// with the existing block — supply all fields you care about). Probes the
/// Grok binary **before** writing when `--enable` is used. Rejects any
/// provider other than `"grok"` (v1 contract).
pub fn handle_set_fallback(db_dir: &Path, opts: SetFallbackOpts) -> io::Result<()> {
    // v1: only "grok" is a valid fallback provider.
    let provider = opts.provider.clone().unwrap_or_else(|| "grok".to_string());
    if !provider.trim().eq_ignore_ascii_case("grok") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unsupported fallback provider {:?} — only \"grok\" is supported in v1 \
                 (allowed: grok)",
                provider
            ),
        ));
    }

    let enabled = opts.enable && !opts.disable;

    // --model is required when --enable is specified.
    if opts.enable && opts.model.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--model is required when --enable is specified \
             (the engine default is \"grok-build\" but it must be explicit here)",
        ));
    }

    let cfg = FallbackRunnerConfig {
        enabled,
        provider: provider.clone(),
        model: opts
            .model
            .clone()
            .unwrap_or_else(|| "grok-build".to_string()),
        cli_binary: opts.cli_binary.clone(),
        runtime_error_threshold: opts.runtime_error_threshold.unwrap_or(2),
    };

    // Probe binary BEFORE write when enabling (CONTRACT: same resolver as
    // loop startup probe — check_fallback_runner_binary in project_config.rs).
    if enabled {
        check_fallback_runner_binary(Some(&cfg)).map_err(|e| io::Error::other(e.to_string()))?;
    }

    let scope = if opts.project { "project" } else { "user" };
    if opts.project {
        write_project_fallback_runner(db_dir, Some(&cfg))?;
    } else {
        write_user_fallback_runner(Some(&cfg))?;
    }
    ui::emit_data(&format!(
        "Set {scope} fallbackRunner: provider={}, model={}, enabled={}",
        cfg.provider, cfg.model, cfg.enabled
    ));
    Ok(())
}

/// `task-mgr models unset-fallback` entry point.
pub fn handle_unset_fallback(db_dir: &Path, project: bool) -> io::Result<()> {
    if project {
        write_project_fallback_runner(db_dir, None)?;
        ui::emit_data("Cleared project fallbackRunner");
    } else {
        write_user_fallback_runner(None)?;
        ui::emit_data("Cleared user fallbackRunner");
    }
    Ok(())
}

#[cfg(test)]
mod show_tests {
    use super::*;
    use crate::db::DbDirSource;
    use crate::loop_engine::model::OPUS_MODEL;
    use std::fs;
    use std::io::Cursor;

    fn show_output(db_dir: &std::path::Path) -> String {
        let mut buf = Cursor::new(Vec::new());
        handle_show_to(&mut buf, db_dir, DbDirSource::CwdDefault).unwrap();
        String::from_utf8(buf.into_inner()).unwrap()
    }

    #[test]
    fn show_full_routing_config_renders_all_sections() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            format!(
                r#"{{
                    "version": 1,
                    "reviewModel": "{OPUS_MODEL}",
                    "fallbackRunner": {{
                        "enabled": true,
                        "provider": "grok",
                        "model": "grok-build",
                        "runtimeErrorThreshold": 2
                    }},
                    "primaryRunner": {{
                        "byBaselineTier": {{
                            "FEAT": {{
                                "opus": {{ "provider": "codex", "fallbackToClaude": true }},
                                "sonnet": {{ "provider": "grok", "model": "grok-build" }}
                            }}
                        }},
                        "byIdPrefix": {{
                            "FIX": {{ "provider": "claude", "model": "{OPUS_MODEL}" }}
                        }}
                    }}
                }}"#
            ),
        )
        .unwrap();

        let out = show_output(dir.path());

        assert!(
            out.contains(&format!("reviewModel:    {OPUS_MODEL} (provider: Claude)")),
            "reviewModel section missing or wrong; got:\n{out}"
        );
        assert!(
            out.contains("fallbackRunner: enabled, provider=grok, model=grok-build, threshold=2"),
            "fallbackRunner section missing or wrong; got:\n{out}"
        );
        assert!(
            out.contains(&format!("byIdPrefix[FIX] -> Claude/{OPUS_MODEL}")),
            "primaryRunner byIdPrefix section missing or wrong; got:\n{out}"
        );
        assert!(
            out.contains("byBaselineTier[FEAT][opus] -> Codex/"),
            "primaryRunner byBaselineTier opus section missing or wrong; got:\n{out}"
        );
        assert!(
            out.contains("byBaselineTier[FEAT][sonnet] -> Grok/grok-build"),
            "primaryRunner byBaselineTier sonnet section missing or wrong; got:\n{out}"
        );
    }

    #[test]
    fn show_empty_config_renders_explicit_empty_states() {
        let dir = tempfile::tempdir().unwrap();

        let out = show_output(dir.path());

        assert!(
            out.contains("reviewModel:    (unset)"),
            "empty reviewModel must print (unset); got:\n{out}"
        );
        assert!(
            out.contains("fallbackRunner: disabled"),
            "absent fallbackRunner must print disabled; got:\n{out}"
        );
        assert!(
            out.contains("primaryRunner:  (no routes)"),
            "absent primaryRunner must print (no routes); got:\n{out}"
        );
    }

    #[test]
    fn show_grok_review_model_shows_grok_provider() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"reviewModel":"grok-build"}"#,
        )
        .unwrap();

        let out = show_output(dir.path());
        assert!(
            out.contains("reviewModel:    grok-build (provider: Grok)"),
            "Grok reviewModel must show Grok provider; got:\n{out}"
        );
    }

    #[test]
    fn show_disabled_fallback_runner_prints_disabled() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"fallbackRunner":{"enabled":false,"provider":"grok","model":"grok-build","runtimeErrorThreshold":2}}"#,
        )
        .unwrap();

        let out = show_output(dir.path());
        assert!(
            out.contains("fallbackRunner: disabled"),
            "disabled fallbackRunner must print disabled; got:\n{out}"
        );
    }

    #[test]
    fn show_primary_runner_by_task_type_and_prefix_sorted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{
                "version": 1,
                "primaryRunner": {
                    "byTaskType": {
                        "review": { "provider": "grok", "model": "grok-build" },
                        "milestone": { "provider": "grok", "model": "grok-build" }
                    },
                    "byIdPrefix": {
                        "REVIEW-": { "provider": "grok", "model": "grok-build" }
                    },
                    "byBaselineTier": {
                        "FEAT": {
                            "opus": { "provider": "codex" },
                            "sonnet": { "provider": "grok", "model": "grok-build" }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let out = show_output(dir.path());
        assert!(
            out.contains("byTaskType[milestone] -> Grok/grok-build"),
            "byTaskType milestone missing; got:\n{out}"
        );
        assert!(
            out.contains("byTaskType[review] -> Grok/grok-build"),
            "byTaskType review missing; got:\n{out}"
        );
        assert!(
            out.contains("byIdPrefix[REVIEW-] -> Grok/grok-build"),
            "byIdPrefix REVIEW- missing; got:\n{out}"
        );
        assert!(
            out.contains("byBaselineTier[FEAT][opus] -> Codex/"),
            "byBaselineTier FEAT opus missing; got:\n{out}"
        );
        assert!(
            out.contains("byBaselineTier[FEAT][sonnet] -> Grok/grok-build"),
            "byBaselineTier FEAT sonnet missing; got:\n{out}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn source_labels_are_stable() {
        assert_eq!(DefaultSource::Project.label(), "project");
        assert_eq!(DefaultSource::User.label(), "user");
        assert_eq!(DefaultSource::None.label(), "none");
    }

    #[test]
    fn list_offline_prints_built_in_models_and_effort_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut buf = Cursor::new(Vec::new());
        handle_list_to(&mut buf, dir.path(), ListOpts::default()).unwrap();
        let out = String::from_utf8(buf.into_inner()).unwrap();
        assert!(out.contains(OPUS_MODEL));
        assert!(out.contains(SONNET_MODEL));
        assert!(out.contains(HAIKU_MODEL));
        assert!(out.contains("Difficulty → --effort mapping"));
        for (d, e) in EFFORT_FOR_DIFFICULTY {
            assert!(out.contains(d));
            assert!(out.contains(e));
        }
    }

    #[test]
    fn list_remote_without_opt_in_falls_back_silently() {
        // Ensure opt-in env is unset for this test.
        let prior_opt = std::env::var_os("TASK_MGR_USE_API");
        unsafe { std::env::remove_var("TASK_MGR_USE_API") };
        let dir = tempfile::tempdir().unwrap();
        let mut buf = Cursor::new(Vec::new());
        handle_list_to(
            &mut buf,
            dir.path(),
            ListOpts {
                remote: true,
                refresh: false,
            },
        )
        .unwrap();
        let out = String::from_utf8(buf.into_inner()).unwrap();
        // Shouldn't hit the network; should have printed the built-in list.
        assert!(out.contains("built-in list"));
        assert!(out.contains(OPUS_MODEL));
        if let Some(v) = prior_opt {
            unsafe { std::env::set_var("TASK_MGR_USE_API", v) };
        }
    }

    #[test]
    fn classify_tier_matches_known_families() {
        // Use constants here so the regression guard (tests/no_hardcoded_models.rs)
        // doesn't flag this file. The classifier works on substring, so the
        // canonical ids exercise every branch.
        assert_eq!(classify_tier(OPUS_MODEL), "Opus");
        assert_eq!(classify_tier(SONNET_MODEL), "Sonnet");
        assert_eq!(classify_tier(HAIKU_MODEL), "Haiku");
        assert_eq!(classify_tier("mystery-model"), "");
    }
}

#[cfg(test)]
mod review_model_tests {
    use super::*;
    use crate::loop_engine::model::SONNET_MODEL;
    use std::fs;

    #[test]
    fn set_review_model_project_scope_writes_key_and_preserves_others() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"embeddingModel":"x"}"#,
        )
        .unwrap();

        // SONNET_MODEL is Claude — no Grok binary probe.
        handle_set_review_model(dir.path(), SONNET_MODEL, true).unwrap();

        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains("\"reviewModel\""), "reviewModel key missing");
        assert!(raw.contains(SONNET_MODEL), "model value missing");
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(raw.contains("embeddingModel"), "embeddingModel lost");
    }

    #[test]
    fn unset_review_model_project_scope_removes_key_preserves_others() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"reviewModel":"grok-build","additionalAllowedTools":["Bash(docker:*)"]}"#,
        )
        .unwrap();

        handle_unset_review_model(dir.path(), true).unwrap();

        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("reviewModel"),
            "reviewModel should be removed: {raw}"
        );
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools preserved"
        );
    }

    #[test]
    fn unset_review_model_on_absent_key_is_noop_success() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        handle_unset_review_model(dir.path(), true).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(!raw.contains("reviewModel"), "key should stay absent");
    }

    #[test]
    fn set_review_model_round_trips_via_unset() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"{"version":1,"embeddingModel":"x"}"#;
        fs::write(dir.path().join("config.json"), original).unwrap();

        handle_set_review_model(dir.path(), SONNET_MODEL, true).unwrap();
        handle_unset_review_model(dir.path(), true).unwrap();

        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("reviewModel"),
            "reviewModel should be removed after round-trip"
        );
        assert!(
            raw.contains("embeddingModel"),
            "other keys must survive round-trip"
        );
    }

    #[test]
    fn set_fallback_enable_without_model_errors() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SetFallbackOpts {
            enable: true,
            disable: false,
            provider: Some("grok".to_string()),
            model: None,
            cli_binary: None,
            runtime_error_threshold: None,
            project: true,
        };
        let result = handle_set_fallback(dir.path(), opts);
        assert!(result.is_err(), "--enable without --model must error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--model"),
            "error must mention --model; got: {msg}"
        );
    }

    #[test]
    fn set_fallback_non_grok_provider_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SetFallbackOpts {
            enable: false,
            disable: true,
            provider: Some("openai".to_string()),
            model: Some("gpt-4".to_string()),
            cli_binary: None,
            runtime_error_threshold: None,
            project: true,
        };
        let result = handle_set_fallback(dir.path(), opts);
        assert!(result.is_err(), "non-grok provider must error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("openai"),
            "error must mention the bad value; got: {msg}"
        );
        assert!(
            msg.contains("grok"),
            "error must mention allowed value; got: {msg}"
        );
    }

    #[test]
    fn set_fallback_probe_before_write_missing_binary_leaves_config_unchanged() {
        use crate::loop_engine::test_utils::GROK_BINARY_MUTEX;
        let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let bogus = "/tmp/task-mgr-test-nonexistent-grok-binary-feat002b-xyz";
        unsafe { std::env::set_var("GROK_BINARY", bogus) };

        let dir = tempfile::tempdir().unwrap();
        let original = r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"]}"#
            .as_bytes()
            .to_vec();
        fs::write(dir.path().join("config.json"), &original).unwrap();

        let opts = SetFallbackOpts {
            enable: true,
            disable: false,
            provider: Some("grok".to_string()),
            model: Some("grok-build".to_string()),
            cli_binary: None,
            runtime_error_threshold: None,
            project: true,
        };
        let result = handle_set_fallback(dir.path(), opts);
        unsafe { std::env::remove_var("GROK_BINARY") };

        assert!(result.is_err(), "missing binary must return Err");
        let after = fs::read(dir.path().join("config.json")).unwrap();
        assert_eq!(after, original, "config must be unchanged on probe failure");
    }

    #[test]
    fn unset_fallback_removes_block_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"fallbackRunner":{"enabled":true,"provider":"grok","model":"grok-build","runtimeErrorThreshold":2}}"#,
        )
        .unwrap();

        handle_unset_fallback(dir.path(), true).unwrap();

        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !raw.contains("fallbackRunner"),
            "fallbackRunner should be removed: {raw}"
        );
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
    }

    #[test]
    fn unset_fallback_on_absent_key_is_noop_success() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), r#"{"version":1}"#).unwrap();
        handle_unset_fallback(dir.path(), true).unwrap();
        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(!raw.contains("fallbackRunner"), "key should stay absent");
    }

    #[test]
    fn set_fallback_disable_flag_writes_enabled_false() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SetFallbackOpts {
            enable: false,
            disable: true,
            provider: Some("grok".to_string()),
            model: Some("grok-build".to_string()),
            cli_binary: None,
            runtime_error_threshold: None,
            project: true,
        };
        handle_set_fallback(dir.path(), opts).unwrap();
        let config = crate::loop_engine::project_config::read_project_config(dir.path());
        let fr = config
            .fallback_runner
            .expect("fallbackRunner should be set");
        assert!(!fr.enabled, "enabled must be false after --disable");
    }

    #[test]
    fn set_fallback_writes_all_provided_fields_and_preserves_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"additionalAllowedTools":["Bash(docker:*)"],"embeddingModel":"x"}"#,
        )
        .unwrap();

        let opts = SetFallbackOpts {
            enable: false,
            disable: true,
            provider: Some("grok".to_string()),
            model: Some("grok-build".to_string()),
            cli_binary: Some("/custom/grok".to_string()),
            runtime_error_threshold: Some(3),
            project: true,
        };
        handle_set_fallback(dir.path(), opts).unwrap();

        let raw = fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains("fallbackRunner"), "fallbackRunner missing");
        assert!(
            raw.contains("additionalAllowedTools"),
            "additionalAllowedTools lost"
        );
        assert!(raw.contains("embeddingModel"), "embeddingModel lost");

        let config = crate::loop_engine::project_config::read_project_config(dir.path());
        let fr = config
            .fallback_runner
            .expect("fallbackRunner should be Some");
        assert_eq!(fr.cli_binary.as_deref(), Some("/custom/grok"));
        assert_eq!(fr.runtime_error_threshold, 3);
    }
}
