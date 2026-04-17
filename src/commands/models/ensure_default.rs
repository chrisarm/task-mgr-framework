//! Orchestrator that resolves a default model, prompting the user when none
//! is set AND the environment permits an interactive flow.
//!
//! Called from two places:
//! 1. `task-mgr init` — after importing a PRD (human-initiated; TTY check only).
//! 2. Loop engine setup — before the first iteration (must additionally skip
//!    when in auto-mode regardless of TTY).
//!
//! Non-interactive runs silently skip with a one-line stderr hint; they never
//! block.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use crate::loop_engine::project_config::read_project_config;
use crate::loop_engine::user_config::{
    read_user_config, write_default_model as write_user_default,
};

use super::api::{RemoteModel, check_opt_in, fetch_models, sort_newest_first};
use super::cache;
use super::picker::{ModelChoice, select_model_interactive};

/// Resolve the default Claude model, prompting the user if nothing is set and
/// the environment is interactive.
///
/// `auto_mode` reflects whether the current command runs under the loop's
/// non-interactive auto mode; set to `true` from loop setup, `false` from
/// `init`. Either value triggers a skip if stdin/stderr aren't TTYs.
///
/// On a successful pick, persists to the **user** config so the choice
/// follows the user across worktrees. Returns the resolved model (or `None`
/// if nothing was set or chosen).
pub fn ensure_default_model(db_dir: &Path, auto_mode: bool) -> Option<String> {
    // 1. Honor any existing preference.
    if let Some(m) = read_project_config(db_dir).default_model {
        return Some(m);
    }
    if let Some(m) = read_user_config().default_model {
        return Some(m);
    }

    // 2. Skip unless we're sure we can safely prompt.
    if auto_mode || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        eprintln!(
            "\x1b[34m[hint]\x1b[0m no default Claude model is set. Run `task-mgr models set-default` to pin one."
        );
        return None;
    }

    // 3. Build the choice list — live API if opted in, else constants.
    let choices = match fetch_choices() {
        Ok(remote) => remote,
        Err(_) => fallback_choices(),
    };

    // 4. Prompt. Use stderr for prompts to avoid polluting piped stdout.
    let stdin = io::stdin();
    let picked =
        select_model_interactive(stdin.lock(), io::stderr().lock(), &choices).unwrap_or(None);

    // 5. Persist if we got a pick.
    if let Some(ref id) = picked
        && let Err(e) = write_user_default(Some(id))
    {
        eprintln!("\x1b[33m[warn]\x1b[0m could not persist default model to user config: {e}");
    }

    picked
}

/// Writable convenience shared by [`ensure_default_model`] and handler tests:
/// fetch or cache the live list, return choices shaped for the picker.
fn fetch_choices() -> Result<Vec<ModelChoice>, super::api::ApiError> {
    check_opt_in()?;
    let models = if let Some(cached) = cache::read_fresh() {
        cached
    } else {
        let mut fresh = fetch_models()?;
        sort_newest_first(&mut fresh);
        cache::write(&fresh);
        fresh
    };
    Ok(models.into_iter().map(remote_to_choice).collect())
}

fn remote_to_choice(m: RemoteModel) -> ModelChoice {
    let tier = tier_label(&m.id);
    let note = m.created_at.map(|dt| dt.format("%Y-%m-%d").to_string());
    ModelChoice {
        id: m.id,
        tier,
        note,
    }
}

fn tier_label(id: &str) -> String {
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

/// Offline fallback: the three tier constants from `loop_engine::model`.
pub fn fallback_choices() -> Vec<ModelChoice> {
    vec![
        ModelChoice {
            id: OPUS_MODEL.to_string(),
            tier: "Opus".to_string(),
            note: None,
        },
        ModelChoice {
            id: SONNET_MODEL.to_string(),
            tier: "Sonnet".to_string(),
            note: None,
        },
        ModelChoice {
            id: HAIKU_MODEL.to_string(),
            tier: "Haiku".to_string(),
            note: None,
        },
    ]
}

/// Variant of the picker used by handlers when we already have a choice list
/// (e.g. `set-default` with no argument). Writes to `writer` for
/// testability; in the production path callers pass `io::stderr()`.
pub fn prompt_with_choices<R: BufRead, W: Write>(
    reader: R,
    writer: W,
    choices: &[ModelChoice],
) -> io::Result<Option<String>> {
    select_model_interactive(reader, writer, choices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_label_classifies_canonical_ids() {
        assert_eq!(tier_label(OPUS_MODEL), "Opus");
        assert_eq!(tier_label(SONNET_MODEL), "Sonnet");
        assert_eq!(tier_label(HAIKU_MODEL), "Haiku");
        assert_eq!(tier_label(&OPUS_MODEL.to_uppercase()), "Opus");
        assert_eq!(tier_label("gpt-4"), "");
    }

    #[test]
    fn fallback_choices_covers_three_tiers() {
        let choices = fallback_choices();
        assert_eq!(choices.len(), 3);
        let ids: Vec<_> = choices.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&OPUS_MODEL));
        assert!(ids.contains(&SONNET_MODEL));
        assert!(ids.contains(&HAIKU_MODEL));
    }
}
