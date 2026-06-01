//! Shared atomic config-file writer used by `project_config` and `user_config`.
//!
//! The sole implementation of the tempfile+rename Value-round-trip pattern lives
//! here; every setter in both config modules delegates to [`write_config_key_at`].

use std::path::Path;

/// Behavior when the existing config file contains malformed JSON.
pub enum OnCorruptJson {
    /// Treat the file as absent and start fresh from `empty_seed`.
    ///
    /// Used by `write_default_model` (tolerant callers where the operator
    /// would prefer a recovery over a hard failure).
    UseSeed,
    /// Return an `Err` that names the file path.
    ///
    /// Used by `write_review_model` and `write_fallback_runner` (strict
    /// callers where a silent overwrite would hide a misconfigured file).
    ReturnError,
}

/// Key-preserving atomic config writer.
///
/// Reads `path` as `serde_json::Value`, mutates one `key` (inserts `value`
/// when `Some` or removes it when `None`), and writes back atomically via a
/// same-directory tempfile + rename. Creates parent directories; seeds from
/// `empty_seed` when the file is absent. Always writes a trailing newline.
///
/// `on_corrupt` controls handling when the file exists but is invalid JSON:
/// - [`OnCorruptJson::UseSeed`] — start fresh from `empty_seed` (tolerant).
/// - [`OnCorruptJson::ReturnError`] — return `Err` with the file path named.
pub fn write_config_key_at(
    path: &Path,
    key: &str,
    value: Option<serde_json::Value>,
    empty_seed: serde_json::Value,
    on_corrupt: OnCorruptJson,
) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut json: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => match on_corrupt {
                OnCorruptJson::UseSeed => empty_seed,
                OnCorruptJson::ReturnError => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{}: malformed JSON: {e}", path.display()),
                    ));
                }
            },
        },
        _ => empty_seed,
    };

    let obj = json.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: config is not a JSON object", path.display()),
        )
    })?;

    match value {
        Some(v) => {
            obj.insert(key.to_string(), v);
        }
        None => {
            obj.remove(key);
        }
    }

    let contents = serde_json::to_string_pretty(&json)
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
