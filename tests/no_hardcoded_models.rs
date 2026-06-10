//! Regression guard: enforces that model strings appear only in the single
//! source of truth — `src/loop_engine/model.rs`.
//!
//! Two patterns, two scopes:
//! - Claude ids (`claude-opus-4-7` / `claude-sonnet-4-6` / `claude-haiku-4-5-*`
//!   / `claude-fable-5`) are banned EVERYWHERE outside `model.rs`. Production
//!   and tests alike already route through the `OPUS_MODEL` / `SONNET_MODEL` /
//!   `HAIKU_MODEL` / `FABLE_MODEL` constants, so this scope stays strict.
//! - Grok ids (`grok-build`, `grok-code-fast-1`, `grok-4*`, …) are banned in
//!   PRODUCTION code only. Unit-test modules (`#[cfg(test)]`) and integration
//!   tests under `tests/` legitimately use the literal `"grok-build"` (there is
//!   no exported Grok constant to import), so the grok check skips test code.
//!   The point is to catch a `grok-build` literal sneaking into a runtime path
//!   (e.g. `default_grok_provider`), which the Claude-only regex used to miss.
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
    // Claude ids: banned everywhere (prod + test). Grok ids: banned in
    // production code only — `grok-build` (no version digit) plus versioned
    // families (`grok-4`, `grok-code-fast-1`). The `grok-(build|code|\d)` shape
    // matches model ids without snaring non-model tokens like `grok-cli`,
    // `grok-fallback`, or `grok-stderr`.
    let claude = regex::Regex::new(r"claude-(opus|sonnet|haiku|fable)-\d").unwrap();
    let grok = regex::Regex::new(r"grok-(build|code|\d)").unwrap();

    let mut offenders: Vec<String> = Vec::new();
    for dir in ["src", "tests"] {
        scan_dir(
            &repo_root.join(dir),
            &repo_root,
            &claude,
            &grok,
            &mut offenders,
        );
    }

    assert!(
        offenders.is_empty(),
        "\nHardcoded model strings found outside the canonical source of truth \
         (`src/loop_engine/model.rs`).\n\
         Claude: use `OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` / `FABLE_MODEL` constants \
         (or `{{{{OPUS_MODEL}}}}` placeholders in `.json.tmpl` fixtures).\n\
         Grok: production code must route model ids through `model.rs` (e.g. the \
         `GROK_DEFAULT_TIER_MODELS` table), never a `\"grok-build\"` literal.\n\n\
         Offenders:\n{}\n",
        offenders.join("\n")
    );
}

#[test]
fn task_generation_docs_do_not_stamp_models_on_feat_tasks() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        ".claude/commands/tasks.md",
        ".claude/commands/plan-tasks.md",
    ] {
        let contents = fs::read_to_string(repo_root.join(rel)).expect("read command doc");
        assert!(
            !contents.contains("Set model: opus ONLY if estimatedEffort"),
            "{rel} must not revive the old FEAT opus-stamping guidance"
        );
        assert!(
            !contents.contains("FEAT-xxx` / `FIX-xxx` with `estimatedEffort"),
            "{rel} must not tell generators to stamp model fields on FEAT tasks"
        );
        assert!(
            contents.contains("Do not stamp `model` on FEAT")
                || contents.contains("Do **not** stamp\n`model` on FEAT")
                || contents.contains("Do not set model; use estimatedEffort"),
            "{rel} must explicitly preserve runtime FEAT provider routing"
        );
    }
}

fn scan_dir(
    dir: &Path,
    repo_root: &Path,
    claude: &regex::Regex,
    grok: &regex::Regex,
    out: &mut Vec<String>,
) {
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
            scan_dir(&path, repo_root, claude, grok, out);
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
        // Integration tests under `tests/` and unit `#[cfg(test)]` modules are
        // all test code → exempt from the grok-id check (they use literals).
        let file_is_test = rel.starts_with("tests/") || rel.contains("/tests/");
        // `#[cfg(test)]`-gated modules are conventionally at the file's end;
        // once we cross that marker, the remainder is test code for grok scoping.
        let mut in_test_region = file_is_test;
        for (lineno, line) in contents.lines().enumerate() {
            if !in_test_region && line.trim_start() == "#[cfg(test)]" {
                in_test_region = true;
            }
            if claude.is_match(line) || (!in_test_region && grok.is_match(line)) {
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
