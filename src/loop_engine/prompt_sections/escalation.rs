//! Escalation policy section builder for the agent loop prompt.
//!
//! Loads the escalation policy template from disk and injects it into the
//! prompt for non-Opus models. Opus is the highest tier — escalation is
//! irrelevant there.

use std::fs;
use std::path::Path;

use crate::loop_engine::model;

/// Load the escalation policy template from the scripts directory.
///
/// Resolves the template path as `base_prompt_path.parent()/scripts/escalation-policy.md`.
/// Returns `Some(content)` if the file exists and is readable, `None` otherwise.
/// Warnings are printed to stderr for read failures (but not for missing files).
///
/// The template is loaded fresh each call (not cached) to allow hot-editing.
pub fn load_escalation_template(base_prompt_path: &Path) -> Option<String> {
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
pub(crate) fn build_escalation_section(
    base_prompt_path: &Path,
    resolved_model: Option<&str>,
) -> String {
    if model::model_tier(resolved_model) == model::ModelTier::Opus {
        return String::new();
    }

    match load_escalation_template(base_prompt_path) {
        Some(contents) => format!("## Model Escalation Policy\n\n{}\n\n---\n\n", contents),
        None => String::new(),
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
}
