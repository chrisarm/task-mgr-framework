//! Per-user configuration living outside the project tree.
//!
//! Path: `$XDG_CONFIG_HOME/task-mgr/config.json` (fallback `~/.config/task-mgr/config.json`).
//! Stores personal preferences that should follow the user across projects.
//!
//! Read-only surface: `read_user_config()` returns defaults when the file is
//! missing or invalid. The only remaining field, `defaultModel`, is DEPRECATED
//! under the provider-first `models`/`routing` config — it never feeds model
//! resolution and is read solely so preflight can emit the deprecation warning
//! (see `project_config::preflight_validate_and_probe`). The legacy writers
//! (`write_default_model` / `write_review_model`) were removed with the
//! FR-001 hard break; the `models` CLI owns all config writes now.

use serde::Deserialize;
use std::path::PathBuf;

use crate::paths::user_config_dir;

/// Per-user configuration.
///
/// Forward-compatible: unknown fields are silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserConfig {
    /// DEPRECATED: ignored by model resolution. Read only to drive the
    /// preflight deprecation warning.
    #[serde(default)]
    pub default_model: Option<String>,
}

/// Return the file path where user config lives, or `None` if neither
/// `XDG_CONFIG_HOME` nor `HOME` is set.
pub fn user_config_path() -> Option<PathBuf> {
    user_config_dir().map(|d| d.join("config.json"))
}

/// Read the user config. Returns defaults when:
/// - `user_config_path()` is `None` (unresolvable path)
/// - the file doesn't exist
/// - the file is unparseable (prints a one-line stderr hint mentioning the path
///   so the user can investigate; never overwrites)
pub fn read_user_config() -> UserConfig {
    let Some(path) = user_config_path() else {
        return UserConfig::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
            eprintln!(
                "\x1b[33m[warn]\x1b[0m user config at {} is unparseable ({e}); using defaults",
                path.display()
            );
            UserConfig::default()
        }),
        Err(_) => UserConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Returns a testable pair: (tempdir, path-to-config-file).
    /// Using a tempdir avoids racing other tests on the real user config.
    fn test_config_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task-mgr/config.json");
        (dir, path)
    }

    #[test]
    fn missing_file_yields_defaults() {
        let (_dir, path) = test_config_path();
        let config = if path.exists() {
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
        } else {
            UserConfig::default()
        };
        assert!(config.default_model.is_none());
    }

    #[test]
    fn unknown_fields_are_ignored_and_default_model_parses() {
        let (_dir, path) = test_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"defaultModel": "some-model", "futureField": {"nested": true}}"#,
        )
        .unwrap();
        let config: UserConfig = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.default_model.as_deref(), Some("some-model"));
    }
}
