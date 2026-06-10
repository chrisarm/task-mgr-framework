//! Anchor-tier picker fired from the `task-mgr init` scaffold path (FR-009).
//!
//! The provider-first redesign replaced the per-user "pick a default model"
//! flow with a project-level "pick an anchor capability tier" flow: the anchor
//! centers the difficulty window (`low → anchor−1, medium → anchor,
//! high → anchor+1`), and on a pick we write the FR-001 default `models`/
//! `routing` block to `.task-mgr/config.json` with that anchor pinned.
//!
//! Fires ONLY when the project config has no `models` block yet AND the
//! environment is interactive (stdin+stderr TTYs, not auto-mode). A config that
//! still carries legacy keys is NOT auto-migrated (FR-002) — we print a hint
//! pointing at `models init --force-replace-legacy` and skip. Non-interactive
//! runs print a one-line stderr hint and skip; they never block.

use std::io::{self, IsTerminal};
use std::path::Path;

use crate::loop_engine::model::{CapabilityTier, Provider, builtin_resolved_models};
use crate::loop_engine::project_config::detect_legacy_model_keys;
use crate::output::ui;

use super::handlers::write_default_block_with_anchor;
use super::picker::{ModelChoice, select_choice_interactive};

/// Ensure the project has a provider-first `models` config, prompting the user
/// to choose an anchor capability tier when nothing is configured and the
/// environment is interactive.
///
/// `auto_mode` reflects the loop's non-interactive auto mode; either it or a
/// non-TTY stdin/stderr triggers a silent skip with a one-line hint. Returns the
/// chosen anchor tier string on a successful pick, else `None`.
pub fn ensure_models_anchor(db_dir: &Path, auto_mode: bool) -> Option<String> {
    let raw = read_raw_config(db_dir);

    // 1. Already configured → nothing to do.
    if raw
        .as_ref()
        .and_then(|v| v.get("models"))
        .is_some_and(|m| !m.is_null())
    {
        return None;
    }

    // 2. Legacy config is NOT auto-migrated (FR-002): instruct, then skip.
    if let Some(v) = raw.as_ref() {
        let legacy = detect_legacy_model_keys(v);
        if !legacy.is_empty() {
            ui::emit(
                "\x1b[34m[hint]\x1b[0m legacy model keys present. Migrate with \
                 `task-mgr models init --force-replace-legacy`.",
            );
            return None;
        }
    }

    // 3. Skip unless we can safely prompt.
    if auto_mode || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        ui::emit(
            "\x1b[34m[hint]\x1b[0m no models config set. Run `task-mgr models init` to scaffold \
             the provider-first config, or `task-mgr models set-anchor <tier>` to pin an anchor.",
        );
        return None;
    }

    // 4. Prompt for an anchor tier (stderr for the prompt, keep stdout clean).
    let choices = anchor_choices();
    let stdin = io::stdin();
    let picked = select_choice_interactive(
        stdin.lock(),
        io::stderr().lock(),
        "Pick an anchor capability tier (centers the difficulty window):",
        &choices,
    )
    .unwrap_or(None);

    // 5. On a pick, write the FR-001 default block with the chosen anchor.
    let id = picked?;
    let anchor = match CapabilityTier::parse(&id) {
        Ok(t) => t,
        // Defensive: the picker only returns ids we put in `anchor_choices`.
        Err(_) => return None,
    };
    if let Err(e) = write_default_block_with_anchor(db_dir, anchor) {
        ui::emit_err(&format!(
            "\x1b[33m[warn]\x1b[0m could not write models config: {e}"
        ));
        return None;
    }
    ui::emit(&format!(
        "Pinned anchor tier `{}` and wrote the FR-001 default models config.",
        anchor.as_str()
    ));
    Some(anchor.as_str().to_string())
}

/// Read the raw project config as a `serde_json::Value`, or `None` when the file
/// is absent / empty / malformed (a malformed config is treated as unconfigured
/// for picker purposes; the mutating verbs surface the parse error separately).
fn read_raw_config(db_dir: &Path) -> Option<serde_json::Value> {
    let contents = std::fs::read_to_string(db_dir.join("config.json")).ok()?;
    if contents.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&contents).ok()
}

/// The four anchor-tier choices, each annotated with the Claude model it resolves
/// to at that tier (from the built-in default ladder) so the operator sees the
/// concrete effect of each choice. `id` is the kebab tier string the picker
/// returns.
fn anchor_choices() -> Vec<ModelChoice> {
    let resolved = builtin_resolved_models();
    CapabilityTier::ALL
        .iter()
        .map(|tier| {
            let example = resolved
                .model_for(Provider::Claude, *tier)
                .unwrap_or("(provider default)");
            ModelChoice {
                id: tier.as_str().to_string(),
                tier: String::new(),
                note: Some(format!("claude → {example}")),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_choices_cover_all_four_tiers() {
        let choices = anchor_choices();
        assert_eq!(choices.len(), 4);
        let ids: Vec<&str> = choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["cheapest", "cost-efficient", "standard", "frontier"]
        );
        // Each parses back to a real tier (round-trip with CapabilityTier::parse).
        for c in &choices {
            assert!(CapabilityTier::parse(&c.id).is_ok(), "{}", c.id);
        }
    }

    #[test]
    fn anchor_choices_annotate_with_resolved_claude_models() {
        let choices = anchor_choices();
        // Every choice shows the claude model it resolves to at that tier.
        for c in &choices {
            let note = c.note.as_ref().expect("note present");
            assert!(note.starts_with("claude → "), "note: {note}");
        }
    }

    #[test]
    fn ensure_skips_when_models_block_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"version":1,"models":{"anchor":"frontier"}}"#,
        )
        .unwrap();
        // Already configured → no prompt, returns None even in auto_mode.
        assert_eq!(ensure_models_anchor(dir.path(), true), None);
    }

    #[test]
    fn ensure_skips_auto_mode_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        // auto_mode=true → skip with hint, never write a models block.
        assert_eq!(ensure_models_anchor(dir.path(), true), None);
        assert!(
            !dir.path().join("config.json").exists(),
            "auto-mode skip must not scaffold a config"
        );
    }
}
