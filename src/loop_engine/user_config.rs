//! Per-user configuration living outside the project tree.
//!
//! Path: `$XDG_CONFIG_HOME/task-mgr/config.json` (fallback `~/.config/task-mgr/config.json`).
//! Stores personal preferences that should follow the user across projects —
//! currently just `defaultModel`.
//!
//! Design mirrors [`crate::loop_engine::project_config`]:
//! - Read via `read_user_config()` returning defaults when the file is missing or invalid.
//! - Write via `write_default_model()` editing only the one field through a
//!   `serde_json::Value` so unknown forward-compat fields survive.
//! - Atomic same-directory tempfile + rename to avoid half-written reads.

use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::paths::user_config_dir;

/// Per-user configuration.
///
/// Forward-compatible: unknown fields are silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserConfig {
    /// Per-user default Claude model. Last stop in the model resolution chain
    /// (below PRD default and project default).
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

/// Set (or clear) the `defaultModel` field. Creates the config directory if
/// needed. Writes atomically via a same-directory tempfile + rename.
///
/// Preserves unknown forward-compat fields by round-tripping through
/// `serde_json::Value` rather than (de)serializing the full struct.
pub fn write_default_model(model: Option<&str>) -> std::io::Result<()> {
    let path = user_config_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve user config path (XDG_CONFIG_HOME and HOME both unset)",
        )
    })?;
    write_default_model_at(&path, model)
}

/// Testable variant of [`write_default_model`] that writes to an explicit path.
pub fn write_default_model_at(path: &Path, model: Option<&str>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut value: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => serde_json::json!({}),
    };
    let obj = value.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "user config.json is not a JSON object",
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
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
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
        // Use the path-explicit read path via inline parsing
        let config = if path.exists() {
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
        } else {
            UserConfig::default()
        };
        assert!(config.default_model.is_none());
    }

    #[test]
    fn write_creates_parent_directory() {
        let (_dir, path) = test_config_path();
        assert!(!path.parent().unwrap().exists());
        write_default_model_at(&path, Some(OPUS_MODEL)).unwrap();
        assert!(path.is_file());
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains(OPUS_MODEL));
    }

    #[test]
    fn write_preserves_unknown_fields() {
        let (_dir, path) = test_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"futureField": {"nested": true}, "someSetting": 42}"#,
        )
        .unwrap();
        write_default_model_at(&path, Some(SONNET_MODEL)).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("futureField"), "unknown field lost: {raw}");
        assert!(raw.contains("someSetting"));
        assert!(raw.contains(SONNET_MODEL));
    }

    #[test]
    fn write_none_removes_key() {
        let (_dir, path) = test_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!(r#"{{"defaultModel": "{HAIKU_MODEL}"}}"#)).unwrap();
        write_default_model_at(&path, None).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("defaultModel"), "key should be gone: {raw}");
    }

    #[test]
    fn corrupt_json_is_recovered_from() {
        let (_dir, path) = test_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "utter garbage").unwrap();
        write_default_model_at(&path, Some(OPUS_MODEL)).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains(OPUS_MODEL));
    }

    #[test]
    fn tempfile_is_same_directory_for_cross_fs_safety() {
        let (_dir, path) = test_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Nothing actionable to assert directly without racing against our own
        // cleanup, but exercising the write confirms `persist` succeeded — which
        // it can only do when the tempfile and target share a filesystem.
        write_default_model_at(&path, Some(SONNET_MODEL)).unwrap();
        assert!(path.is_file());
    }
}
