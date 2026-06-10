//! Escalation policy section builder for the agent loop prompt.
//!
//! Loads the escalation policy template from disk and injects it into the
//! prompt unless the model already sits at the ceiling (frontier) Claude tier —
//! there is nothing higher to escalate to, so the policy is irrelevant there.

use std::fs;
use std::path::Path;

use crate::loop_engine::model;
use crate::loop_engine::prompt::assembler::{PromptContext, Rendered, SectionKind, SectionSpec};

/// Stable section identifier for the escalation-policy section. Matches the
/// `section_sizes` key the sequential builder already uses for this section.
pub const ESCALATION_SECTION: &str = "escalation";

/// Load the escalation policy template from the scripts directory.
///
/// Resolves the template path as `base_prompt_path.parent()/scripts/escalation-policy.md`.
/// Returns `Some(content)` if the file exists and is readable, `None` otherwise.
/// Warnings are printed to stderr for read failures (but not for missing files).
///
/// The template is loaded fresh each call (not cached) to allow hot-editing.
pub(crate) fn load_escalation_template(base_prompt_path: &Path) -> Option<String> {
    let parent = base_prompt_path.parent().unwrap_or_else(|| Path::new("."));
    let template_path = parent.join("scripts").join("escalation-policy.md");

    match fs::read_to_string(&template_path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!(
                "Warning: could not read escalation template {}: {}",
                template_path.display(),
                e
            );
            None
        }
    }
}

/// Build an escalation policy section string.
///
/// Omitted when the model is already at the ceiling (frontier) Claude tier —
/// there is no higher tier to escalate to. Tier membership is config
/// exact-match via [`model::ResolvedModelsConfig::tier_of`] against the builtin
/// Claude ladder (no substring matching); a model off the Claude ladder
/// (`tier_of == None`, e.g. an unknown id) is not at the ceiling and still
/// receives the policy.
pub fn build_escalation_section(base_prompt_path: &Path, resolved_model: Option<&str>) -> String {
    build_escalation_section_with_config(
        base_prompt_path,
        resolved_model,
        model::builtin_resolved_models(),
    )
}

/// Operator-config-aware variant of [`build_escalation_section`]: the ceiling
/// check resolves the Claude tier ladder from the supplied `models` config
/// instead of the builtin defaults.
///
/// This is the production path (REFACTOR-007): [`render_escalation_section`]
/// passes the operator-resolved config carried on [`PromptContext`], so an
/// operator who remapped the Claude frontier rung sees the policy omitted at
/// THEIR ceiling — not the builtin one. The 2-arg [`build_escalation_section`]
/// is retained for the equivalence tests that exercise the default ladder.
pub fn build_escalation_section_with_config(
    base_prompt_path: &Path,
    resolved_model: Option<&str>,
    models: &model::ResolvedModelsConfig,
) -> String {
    let at_ceiling = resolved_model
        .and_then(|m| models.tier_of(model::Provider::Claude, m))
        .is_some_and(|t| t == model::CapabilityTier::Frontier);
    if at_ceiling {
        return String::new();
    }

    match load_escalation_template(base_prompt_path) {
        Some(contents) => format!("## Model Escalation Policy\n\n{}\n\n---\n\n", contents),
        None => String::new(),
    }
}

/// Render the escalation-policy section for the data-driven assembler
/// (CONTRACT-001). Sequential-only — the slot roster omits it (wave slots drop
/// escalation by design). This is the **single render site**; it wraps
/// [`build_escalation_section`], reading the resolved model from
/// [`PromptContext::resolved_model`] (the policy is omitted for the Opus tier).
/// A CRITICAL section: it is gated with the other criticals against the total
/// budget, never trimmed. The [`SectionKind`] argument is therefore ignored.
pub fn render_escalation_section(ctx: &PromptContext<'_>, _kind: SectionKind) -> Rendered {
    Rendered {
        text: build_escalation_section_with_config(
            ctx.base_prompt_path,
            ctx.resolved_model,
            ctx.resolved_models,
        ),
        ..Default::default()
    }
}

/// Build the escalation [`SectionSpec`] (critical).
///
/// Present in the sequential roster only. Critical because the escalation
/// policy is part of the non-trimmable critical envelope in the sequential
/// budget gate (its bytes counted toward the overflow check pre-migration).
pub fn escalation_spec() -> SectionSpec {
    SectionSpec {
        name: ESCALATION_SECTION,
        kind: SectionKind::Critical,
        render: render_escalation_section,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use tempfile::TempDir;

    /// Helper: create escalation template file under base_prompt_path's parent/scripts/.
    fn create_escalation_template(base_prompt_dir: &Path, content: &str) -> std::path::PathBuf {
        let scripts_dir = base_prompt_dir.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        let template_path = scripts_dir.join("escalation-policy.md");
        fs::write(&template_path, content).unwrap();
        template_path
    }

    // --- AC1: load_escalation_template with valid file returns Some(content) ---

    #[test]
    fn test_load_escalation_template_valid_file_returns_some() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();

        let template_content = "# Escalation Policy\n\nWhen stuck, escalate to opus.\n";
        create_escalation_template(temp_dir.path(), template_content);

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some(template_content.to_string()),
            "Valid template file should return Some(content)"
        );
    }

    // --- AC2: load_escalation_template with missing file returns None ---

    #[test]
    fn test_load_escalation_template_missing_file_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        // No escalation template created

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(result, None, "Missing template file should return None");
    }

    // --- Edge case: empty template file returns Some("") ---

    #[test]
    fn test_load_escalation_template_empty_file_returns_some_empty() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        create_escalation_template(temp_dir.path(), "");

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some(String::new()),
            "Empty template file should return Some('') — harmless empty section"
        );
    }

    // --- Edge case: template with unicode content passes through verbatim ---

    #[test]
    fn test_load_escalation_template_unicode_content_verbatim() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        let unicode_content = "# エスカレーション方針\n\n困った場合は opus に切り替え 🚀\n";
        create_escalation_template(temp_dir.path(), unicode_content);

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some(unicode_content.to_string()),
            "Unicode content should pass through verbatim"
        );
    }

    // --- Edge case: base_prompt_path with no parent (just filename) ---

    #[test]
    fn test_load_escalation_template_no_parent_resolves_relative() {
        // When base_prompt_path is just "prompt.md" with no parent directory,
        // the function should resolve relative to "." (current dir).
        // This must not panic regardless of whether the file exists.
        let bare_path = Path::new("prompt.md");
        let _result = load_escalation_template(bare_path);
        // No panic = pass. Result depends on whether scripts/escalation-policy.md
        // exists in cwd, which is environment-dependent.
    }

    // --- Invariant: Template is loaded fresh each call (not cached) ---

    #[test]
    fn test_load_escalation_template_not_cached_hot_edit() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");

        // First call: template with initial content
        create_escalation_template(temp_dir.path(), "version 1");
        let result1 = load_escalation_template(&base_prompt_path);
        assert_eq!(result1, Some("version 1".to_string()));

        // Hot-edit: overwrite with new content
        create_escalation_template(temp_dir.path(), "version 2 — hot edited");
        let result2 = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result2,
            Some("version 2 — hot edited".to_string()),
            "Template should be re-read each call, not cached"
        );
    }

    /// AC: Large template content (>5KB) loads and injects correctly.
    #[test]
    fn test_escalation_large_template_loads_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();

        let large_content = "x".repeat(6000);
        create_escalation_template(temp_dir.path(), &large_content);

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result.as_ref().map(|s| s.len()),
            Some(6000),
            "Large template (>5KB) should load fully without truncation"
        );
    }

    /// AC: base_prompt_path in different directory → template resolves from correct parent.
    #[test]
    fn test_escalation_template_different_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("config").join("prompts");
        fs::create_dir_all(&subdir).unwrap();

        let base_prompt_path = subdir.join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();

        // Template must be under subdir/scripts/, not temp_dir/scripts/
        let scripts_dir = subdir.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        fs::write(
            scripts_dir.join("escalation-policy.md"),
            "Nested template content",
        )
        .unwrap();

        let result = load_escalation_template(&base_prompt_path);
        assert_eq!(
            result,
            Some("Nested template content".to_string()),
            "Template should resolve relative to base_prompt_path's parent, not cwd"
        );
    }

    /// REFACTOR-007: the at-ceiling check resolves the Claude ladder from the
    /// OPERATOR config, not the builtin defaults. With a custom config where
    /// opus is the frontier rung, an opus-tier task is at the ceiling → the
    /// policy section is omitted; the builtin ladder (opus = standard, fable =
    /// frontier) would still emit it. Proves
    /// `build_escalation_section_with_config` honors the threaded config.
    #[test]
    fn test_escalation_ceiling_uses_operator_ladder_not_builtin() {
        use crate::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};

        let temp_dir = TempDir::new().unwrap();
        let base_prompt_path = temp_dir.path().join("prompt.md");
        fs::write(&base_prompt_path, "base prompt").unwrap();
        create_escalation_template(temp_dir.path(), "escalate when stuck");

        // Operator config: opus is the frontier rung (no fable defined).
        let json = serde_json::json!({
            "primaryProvider": "claude",
            "anchor": "standard",
            "providers": {
                "claude": {
                    "enabled": true,
                    "tiers": { "cost-efficient": SONNET_MODEL, "frontier": OPUS_MODEL }
                }
            }
        });
        let models: crate::loop_engine::project_config::ModelsConfig =
            serde_json::from_value(json).expect("production-shaped models JSON deserializes");
        let operator = model::resolve_models_config(
            &models,
            &crate::loop_engine::project_config::RoutingConfig::default(),
        );

        // Operator ladder: opus IS the frontier → at ceiling → section omitted.
        let operator_section =
            build_escalation_section_with_config(&base_prompt_path, Some(OPUS_MODEL), &operator);
        assert_eq!(
            operator_section, "",
            "opus is the operator frontier rung → escalation policy must be omitted"
        );

        // Builtin ladder: opus is `standard` (fable is frontier) → NOT at the
        // ceiling → the policy section renders from the template.
        let builtin_section = build_escalation_section_with_config(
            &base_prompt_path,
            Some(OPUS_MODEL),
            model::builtin_resolved_models(),
        );
        assert!(
            builtin_section.contains("Model Escalation Policy"),
            "builtin ladder: opus is below the frontier → policy must still render"
        );
    }
}
