//! Shared test helpers for integration tests.
//!
//! Rust's convention: files under `tests/common/` don't each get compiled as
//! a separate test binary (unlike top-level `tests/*.rs`). Integration tests
//! pull this in with `mod common;`.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};

/// Absolute path to `tests/fixtures/`.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Prepare a fixture for a test: read `<name>.tmpl` (with placeholder
/// substitution) if it exists, else the plain `<name>` (passthrough). Writes
/// the final content to `dest_dir/<name>` and returns that path.
///
/// Placeholders (`{{OPUS_MODEL}}` / `{{SONNET_MODEL}}` / `{{HAIKU_MODEL}}`)
/// resolve to the canonical constants in `src/loop_engine/model.rs`.
pub fn render_fixture_tmpl(name: &str, dest_dir: &Path) -> PathBuf {
    let tmpl_path = fixtures_dir().join(format!("{name}.tmpl"));
    let plain_path = fixtures_dir().join(name);
    let (source, is_tmpl) = if tmpl_path.is_file() {
        (tmpl_path, true)
    } else if plain_path.is_file() {
        (plain_path, false)
    } else {
        panic!(
            "render_fixture_tmpl: neither {} nor {} exists",
            fixtures_dir().join(format!("{name}.tmpl")).display(),
            fixtures_dir().join(name).display(),
        );
    };
    let raw = fs::read_to_string(&source)
        .unwrap_or_else(|e| panic!("render_fixture_tmpl: read {}: {e}", source.display()));
    let rendered = if is_tmpl {
        substitute_placeholders(&raw)
            .unwrap_or_else(|e| panic!("render_fixture_tmpl: {} in {}", e, source.display()))
    } else {
        raw
    };
    let dest = dest_dir.join(name);
    fs::write(&dest, rendered)
        .unwrap_or_else(|e| panic!("render_fixture_tmpl: write {}: {e}", dest.display()));
    dest
}

fn substitute_placeholders(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let end = after_start
            .find("}}")
            .ok_or_else(|| format!("unterminated `{{{{` placeholder starting at byte {start}"))?;
        let name = after_start[..end].trim();
        let replacement = match name {
            "OPUS_MODEL" => OPUS_MODEL,
            "SONNET_MODEL" => SONNET_MODEL,
            "HAIKU_MODEL" => HAIKU_MODEL,
            other => return Err(format!("unknown placeholder `{{{{{other}}}}}`")),
        };
        out.push_str(replacement);
        rest = &after_start[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_known_placeholders() {
        let out = substitute_placeholders("a {{OPUS_MODEL}} b {{HAIKU_MODEL}} c").unwrap();
        assert_eq!(out, format!("a {OPUS_MODEL} b {HAIKU_MODEL} c"));
    }

    #[test]
    fn unknown_placeholder_errors() {
        let err = substitute_placeholders("{{NOPE_MODEL}}").unwrap_err();
        assert!(err.contains("NOPE_MODEL"));
    }

    #[test]
    fn unterminated_placeholder_errors() {
        let err = substitute_placeholders("prefix {{OPUS_MODEL").unwrap_err();
        assert!(err.contains("unterminated"));
    }

    #[test]
    fn no_placeholders_passes_through() {
        let s = "plain json content";
        assert_eq!(substitute_placeholders(s).unwrap(), s);
    }
}
