//! Shared atomic config-file writer.
//!
//! The sole implementation of the tempfile+rename Value-round-trip pattern
//! lives here. The per-key writer (`write_config_key_at`) and its
//! `OnCorruptJson` policy enum were removed with the legacy
//! `defaultModel`/`reviewModel`/`fallbackRunner` setters (FR-001 hard break);
//! the `models` CLI mutates the parsed document directly and persists it
//! through [`write_config_value_at`].

use std::path::Path;

/// Atomically write an entire `serde_json::Value` to `path` (pretty-printed,
/// trailing newline) via a same-directory tempfile + rename, creating parent
/// directories as needed. The nested `models`/`routing` setters in
/// `commands::models` use this after mutating a deep path in the parsed value,
/// so unrelated top-level AND nested keys are preserved verbatim.
pub fn write_config_value_at(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(value)
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
