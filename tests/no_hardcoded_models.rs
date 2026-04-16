//! Regression guard: enforces that Claude model strings like
//! `claude-opus-4-7` / `claude-sonnet-4-6` / `claude-haiku-4-5-*` appear only
//! in the single source of truth — `src/loop_engine/model.rs`.
//!
//! See `src/loop_engine/model.rs` for why; see `.claude/commands/tasks.md`
//! and `tests/fixtures/*.json.tmpl` for how other call sites derive from it.

use std::fs;
use std::path::{Path, PathBuf};

/// Files and paths that are allowed to contain literal model strings.
/// Paths are matched as suffixes to be both portable and strict.
const WHITELIST_SUFFIXES: &[&str] = &[
    // Canonical source of truth.
    "src/loop_engine/model.rs",
    // This test file itself (contains the regex pattern as a literal).
    "tests/no_hardcoded_models.rs",
    // Shell fixture whose model string is arbitrary test data (annotated).
    "tests/fixtures/mock_stream_json.sh",
];

/// Directories to exclude entirely (templates, generated docs, build output).
const SKIP_DIR_SUFFIXES: &[&str] = &["target", ".git"];

/// File extensions whose rendered content is *produced from* the canonical
/// constants (e.g., `.tmpl` fixtures use placeholders, not literals — but we
/// still scan them to make sure no one slipped a literal in by accident).
/// We scan every text file; `.tmpl` legitimately contains only placeholders.
const SCAN_EXTENSIONS: &[&str] = &["rs", "json", "md", "tmpl", "toml", "yml", "yaml"];

#[test]
fn no_hardcoded_model_strings_outside_model_rs() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pattern = regex::Regex::new(r"claude-(opus|sonnet|haiku)-\d").unwrap();

    let mut offenders: Vec<String> = Vec::new();
    for dir in ["src", "tests"] {
        scan_dir(
            &repo_root.join(dir),
            &repo_root,
            &pattern,
            &mut offenders,
        );
    }

    assert!(
        offenders.is_empty(),
        "\nHardcoded Claude model strings found outside the canonical source of truth \
         (`src/loop_engine/model.rs`).\n\
         Use `OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` constants or \
         `{{{{OPUS_MODEL}}}}` placeholders in `.json.tmpl` fixtures instead.\n\n\
         Offenders:\n{}\n",
        offenders.join("\n")
    );
}

fn scan_dir(dir: &Path, repo_root: &Path, pattern: &regex::Regex, out: &mut Vec<String>) {
    if SKIP_DIR_SUFFIXES
        .iter()
        .any(|s| dir.ends_with(s) || dir.to_string_lossy().contains(&format!("/{s}/")))
    {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, repo_root, pattern, out);
            continue;
        }
        if !has_scanned_extension(&path) {
            continue;
        }
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if WHITELIST_SUFFIXES.iter().any(|s| rel.ends_with(s)) {
            continue;
        }
        let contents = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue, // binary or unreadable — skip
        };
        for (lineno, line) in contents.lines().enumerate() {
            if pattern.is_match(line) {
                out.push(format!("  {}:{}: {}", rel, lineno + 1, line.trim()));
            }
        }
    }
}

fn has_scanned_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SCAN_EXTENSIONS.contains(&e))
        .unwrap_or(false)
}
