//! Regenerates the MODELS block in `.claude/commands/tasks.md` from the
//! canonical source-of-truth constants in `src/loop_engine/model.rs`.
//!
//! Usage:
//!   cargo run --bin gen-docs              # rewrite the doc in-place
//!   cargo run --bin gen-docs -- --check   # exit 1 with a diff if stale
//!
//! See `src/loop_engine/model.rs` for why this exists.

use regex::Regex;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const MODELS_BEGIN: &str = "<!-- MODELS:BEGIN -->";
const MODELS_END: &str = "<!-- MODELS:END -->";
const EXPECTED_MODEL_CONSTS: &[&str] = &["OPUS_MODEL", "SONNET_MODEL", "HAIKU_MODEL"];

fn main() -> ExitCode {
    let check_mode = std::env::args().any(|a| a == "--check");

    let root = match repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gen-docs: could not locate repo root: {e}");
            return ExitCode::from(2);
        }
    };
    let model_rs = root.join("src/loop_engine/model.rs");
    let tasks_md = root.join(".claude/commands/tasks.md");

    let block = match render_block(&model_rs) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("gen-docs: failed to render block from {}: {e}", model_rs.display());
            return ExitCode::from(2);
        }
    };

    let current = match fs::read_to_string(&tasks_md) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "gen-docs: could not read {}: {e}. Create the file and add {MODELS_BEGIN} / {MODELS_END} markers.",
                tasks_md.display()
            );
            return ExitCode::from(2);
        }
    };

    let new_contents = match splice_block(&current, &block) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gen-docs: {e} (in {})", tasks_md.display());
            return ExitCode::from(2);
        }
    };

    if new_contents == current {
        if !check_mode {
            println!("gen-docs: {} already up to date", tasks_md.display());
        }
        return ExitCode::SUCCESS;
    }

    if check_mode {
        eprintln!(
            "gen-docs: {} is stale. Run `cargo run --bin gen-docs` to regenerate.",
            tasks_md.display()
        );
        eprintln!("--- expected block ---\n{block}\n--- end ---");
        return ExitCode::from(1);
    }

    if let Err(e) = write_atomic(&tasks_md, &new_contents) {
        eprintln!("gen-docs: failed to write {}: {e}", tasks_md.display());
        return ExitCode::from(2);
    }
    println!("gen-docs: updated {}", tasks_md.display());
    ExitCode::SUCCESS
}

/// Walk up from CARGO_MANIFEST_DIR (or CWD as fallback) to find the repo root
/// (identified by `Cargo.toml` + `src/loop_engine/model.rs`).
fn repo_root() -> Result<PathBuf, String> {
    let start = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_dir().map_err(|e| e.to_string()))?;
    let mut cur = start.as_path();
    loop {
        if cur.join("Cargo.toml").is_file() && cur.join("src/loop_engine/model.rs").is_file() {
            return Ok(cur.to_path_buf());
        }
        cur = cur
            .parent()
            .ok_or_else(|| format!("walked past filesystem root from {}", start.display()))?;
    }
}

/// Extract model constants + effort table from model.rs and render the
/// markdown block (without the surrounding BEGIN/END markers).
fn render_block(model_rs: &Path) -> Result<String, String> {
    let source = fs::read_to_string(model_rs).map_err(|e| e.to_string())?;

    let model_re = Regex::new(r#"pub const (\w+): &str = "([^"]+)";"#).unwrap();
    let mut models: Vec<(String, String)> = model_re
        .captures_iter(&source)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .filter(|(name, _)| EXPECTED_MODEL_CONSTS.contains(&name.as_str()))
        .collect();
    // Preserve canonical order (Opus, Sonnet, Haiku) regardless of file order.
    models.sort_by_key(|(name, _)| {
        EXPECTED_MODEL_CONSTS
            .iter()
            .position(|n| n == name)
            .unwrap_or(usize::MAX)
    });
    if models.len() != EXPECTED_MODEL_CONSTS.len() {
        return Err(format!(
            "expected {} model constants ({:?}), found {} ({:?})",
            EXPECTED_MODEL_CONSTS.len(),
            EXPECTED_MODEL_CONSTS,
            models.len(),
            models.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        ));
    }

    // Parse EFFORT_FOR_DIFFICULTY: &[("difficulty", "effort"), ...];
    let effort_re = Regex::new(
        r"pub const EFFORT_FOR_DIFFICULTY:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(?P<body>[^\]]+)\]",
    )
    .unwrap();
    let body = effort_re
        .captures(&source)
        .ok_or_else(|| "could not find EFFORT_FOR_DIFFICULTY constant".to_string())?
        .name("body")
        .unwrap()
        .as_str();
    let row_re = Regex::new(r#"\("([^"]+)",\s*"([^"]+)"\)"#).unwrap();
    let effort: Vec<(String, String)> = row_re
        .captures_iter(body)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    if effort.is_empty() {
        return Err("EFFORT_FOR_DIFFICULTY contains no rows".into());
    }

    let mut out = String::new();
    out.push_str("<!-- This block is auto-generated by `cargo run --bin gen-docs` from src/loop_engine/model.rs. Do not edit by hand. -->\n");
    out.push('\n');
    out.push_str("**Current model IDs** (bumped in `src/loop_engine/model.rs`):\n");
    out.push('\n');
    for (name, value) in &models {
        let tier = match name.as_str() {
            "OPUS_MODEL" => "Opus",
            "SONNET_MODEL" => "Sonnet",
            "HAIKU_MODEL" => "Haiku",
            _ => name.as_str(),
        };
        out.push_str(&format!("- **{tier}** → `{name}` = `{value}`\n"));
    }
    out.push('\n');
    out.push_str("**Difficulty → `--effort` mapping** (from `EFFORT_FOR_DIFFICULTY`):\n");
    out.push('\n');
    for (difficulty, e) in &effort {
        out.push_str(&format!("- `{difficulty}` → `{e}`\n"));
    }
    Ok(out)
}

/// Replace the content between BEGIN/END markers. Fails if markers are
/// missing or appear more than once.
fn splice_block(current: &str, block: &str) -> Result<String, String> {
    let begin_count = current.matches(MODELS_BEGIN).count();
    let end_count = current.matches(MODELS_END).count();
    if begin_count == 0 || end_count == 0 {
        return Err(format!(
            "expected {MODELS_BEGIN} and {MODELS_END} markers; found {begin_count} begin / {end_count} end"
        ));
    }
    if begin_count > 1 || end_count > 1 {
        return Err(format!(
            "ambiguous markers: expected exactly one pair, found {begin_count} begin / {end_count} end"
        ));
    }
    let begin_idx = current.find(MODELS_BEGIN).unwrap();
    let end_idx = current.find(MODELS_END).unwrap();
    if end_idx < begin_idx {
        return Err("END marker appears before BEGIN marker".into());
    }
    let after_begin = begin_idx + MODELS_BEGIN.len();
    let mut result = String::with_capacity(current.len() + block.len());
    result.push_str(&current[..after_begin]);
    result.push('\n');
    result.push_str(block);
    if !block.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(&current[end_idx..]);
    Ok(result)
}

/// Write atomically via tempfile + rename in the same directory.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_replaces_between_markers() {
        let input = "head\n<!-- MODELS:BEGIN -->\nOLD\n<!-- MODELS:END -->\ntail\n";
        let out = splice_block(input, "NEW\n").unwrap();
        assert!(out.contains("NEW"));
        assert!(!out.contains("OLD"));
        assert!(out.contains("head"));
        assert!(out.contains("tail"));
    }

    #[test]
    fn splice_missing_markers_errors() {
        let err = splice_block("no markers here", "X").unwrap_err();
        assert!(err.contains("MODELS:BEGIN"));
    }

    #[test]
    fn splice_duplicate_markers_errors() {
        let input = "<!-- MODELS:BEGIN -->\nA\n<!-- MODELS:END -->\n<!-- MODELS:BEGIN -->\nB\n<!-- MODELS:END -->\n";
        let err = splice_block(input, "X").unwrap_err();
        assert!(err.contains("ambiguous"));
    }

    #[test]
    fn splice_is_idempotent() {
        let input = "head\n<!-- MODELS:BEGIN -->\nBLOCK\n<!-- MODELS:END -->\ntail\n";
        let once = splice_block(input, "BLOCK\n").unwrap();
        let twice = splice_block(&once, "BLOCK\n").unwrap();
        assert_eq!(once, twice);
    }
}
