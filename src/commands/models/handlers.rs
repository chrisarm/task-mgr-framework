//! Command handlers for `task-mgr models <list|set-default|unset-default|show>`.

use std::io;
use std::path::Path;

use super::api::{ApiError, check_opt_in, fetch_models, sort_newest_first};
use super::cache;
use super::ensure_default::{fallback_choices, prompt_with_choices};
use super::picker::ModelChoice;
use crate::loop_engine::model::{EFFORT_FOR_DIFFICULTY, HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use crate::loop_engine::project_config::{
    read_project_config, write_default_model as write_project_default,
};
use crate::loop_engine::user_config::{
    read_user_config, write_default_model as write_user_default,
};

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
                eprintln!("\x1b[33m[warn]\x1b[0m live model fetch failed: {e}; using offline list");
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
        println!("(no default set)");
        return Ok(());
    };
    if opts.project {
        write_project_default(db_dir, Some(&id))?;
        println!("Set project default model to {id}");
    } else {
        write_user_default(Some(&id))?;
        println!("Set user default model to {id}");
    }
    Ok(())
}

/// `task-mgr models unset-default` entry point.
pub fn handle_unset_default(db_dir: &Path, opts: UnsetDefaultOpts) -> io::Result<()> {
    if opts.project {
        write_project_default(db_dir, None)?;
        println!("Cleared project default model");
    } else {
        write_user_default(None)?;
        println!("Cleared user default model");
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
    let project = read_project_config(db_dir).default_model;
    let user = read_user_config().default_model;
    let (model, source) = match (project.as_ref(), user.as_ref()) {
        (Some(p), _) => (Some(p.clone()), DefaultSource::Project),
        (None, Some(u)) => (Some(u.clone()), DefaultSource::User),
        (None, None) => (None, DefaultSource::None),
    };
    match model {
        Some(m) => println!("Default model: {m}  (source: {})", source.label()),
        None => println!(
            "No default model set. Pick one with `task-mgr models set-default`, \
             or rely on per-PRD / per-task overrides."
        ),
    }
    println!(
        "db_dir: {}  (source: {})",
        db_dir.display(),
        db_dir_source.label(),
    );
    Ok(())
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
