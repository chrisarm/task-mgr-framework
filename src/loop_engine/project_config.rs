use serde::Deserialize;
use std::path::Path;

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
    /// When set, overrides the default scoped mode (but env vars still win).
    #[serde(default)]
    pub permission_mode: Option<String>,

    /// Ollama server URL for embedding generation.
    /// Defaults to `http://localhost:11434`.
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
        }
    }
}

fn default_version() -> u32 {
    1
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
}
