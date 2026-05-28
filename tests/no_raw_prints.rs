//! CI guard (CONTRACT-LOG-001): raw `println!` / `eprintln!` are confined to
//! `src/output/` (the `ui::` product-channel home). Every other module routes
//! human/operator/CLI output through `crate::output::ui::*` and internal
//! diagnostics through `tracing` (see `src/observability.rs`).
//!
//! This guard fails the build if a raw print macro appears in production code
//! **outside `src/output/`** in a module that is NOT on the allow-list.
//!
//! ## The allow-list shrinks per migration
//!
//! [`ALLOWLIST_SUFFIXES`] seeds with EVERY module that still contains a raw
//! print at FEAT-001, so the guard is green before any migration. Each
//! migration FEAT (FEAT-002..006) removes the modules it cleans up. When the
//! list is empty, every print outside `src/output/` has been classified into a
//! `ui::` call (channel A/A2) or a `tracing` event (channel B).
//!
//! ## Why a word boundary matters (known-bad)
//!
//! A naive `line.contains("println!")` would also match `eprintln!` (it ends in
//! `println!`), mis-counting every error print as a `println!` offender. The
//! detector here anchors on a macro-name boundary so `eprintln!` and `println!`
//! are distinguished — see [`tests::eprintln_and_println_are_distinguished`].
//!
//! Mirrors `tests/no_hardcoded_models.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

/// Modules (path suffixes, forward-slash) permitted to retain raw print macros
/// in production code until their migration task lands. SEEDED with every
/// offending module at FEAT-001; shrinks as FEAT-002..006 migrate each module.
///
/// `src/output/` is exempt structurally (it is the `ui::` home) and never
/// appears here. `src/bin/gen-docs.rs` is a developer doc-generator binary, not
/// part of the loop runtime, but is migrated like any other module.
const ALLOWLIST_SUFFIXES: &[&str] = &[
    "src/bin/gen-docs.rs",
    "src/cli/introspect.rs",
    // FEAT-005 migrated src/main.rs, src/handlers.rs, and all of src/commands/**
    // to ui:: / tracing. `curate/tests.rs` stays: it is a `#[cfg(test)] mod tests;`
    // file whose raw prints are test diagnostics (libtest-captured eprintln!),
    // not production output — strip_cfg_test does not strip a whole standalone
    // test file, so the guard would otherwise flag it.
    "src/commands/curate/tests.rs",
    "src/db/connection.rs",
    "src/db/schema/key_decisions.rs",
    "src/learnings/crud/writer.rs",
    "src/learnings/embeddings/mod.rs",
    "src/learnings/ingestion/extraction.rs",
    "src/learnings/ingestion/mod.rs",
    "src/lifecycle/apply/complete.rs",
    "src/lifecycle/apply/mod.rs",
    "src/lifecycle/plan_apply.rs",
    "src/loop_engine/auto_review.rs",
    "src/loop_engine/branch.rs",
    "src/loop_engine/calibrate.rs",
    "src/loop_engine/claude.rs",
    "src/loop_engine/config.rs",
    "src/loop_engine/context.rs",
    "src/loop_engine/deadline.rs",
    "src/loop_engine/display.rs",
    "src/loop_engine/engine.rs",
    "src/loop_engine/env.rs",
    "src/loop_engine/feedback.rs",
    "src/loop_engine/iteration.rs",
    "src/loop_engine/iteration_pipeline.rs",
    "src/loop_engine/monitor.rs",
    "src/loop_engine/oauth.rs",
    "src/loop_engine/orchestrator.rs",
    "src/loop_engine/overflow.rs",
    // FEAT-004 migrated prd_reconcile.rs, batch.rs, usage.rs, signals.rs, and
    // progress.rs to ui:: / tracing (the byte-locked PRD-sync warning lives in
    // engine.rs / lifecycle, not here, so lifecycle_stderr_contract.rs is
    // untouched by this batch).
    "src/loop_engine/project_config.rs",
    "src/loop_engine/prompt/core.rs",
    "src/loop_engine/prompt/sequential.rs",
    "src/loop_engine/prompt/slot.rs",
    "src/loop_engine/prompt_sections/escalation.rs",
    "src/loop_engine/prompt_sections/learnings.rs",
    "src/loop_engine/prompt_sections/mod.rs",
    "src/loop_engine/recovery.rs",
    "src/loop_engine/runner.rs",
    "src/loop_engine/stream.rs",
    "src/loop_engine/user_config.rs",
    "src/loop_engine/watchdog.rs",
];

/// Directories to skip entirely.
const SKIP_DIR_SUFFIXES: &[&str] = &["target", ".git"];

/// The `ui::` home — raw print macros are expected here and never flagged.
const OUTPUT_HOME_PREFIX: &str = "src/output/";

/// Macro-name-boundary regex for `println!` / `eprintln!`. The leading
/// `(?:^|[^A-Za-z0-9_])` is the boundary: `println` inside `eprintln` is NOT
/// preceded by a boundary char (it is preceded by `e`), so a `println!` match
/// cannot swallow an `eprintln!`. Capture group 1 names the macro.
fn print_macro_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[^A-Za-z0-9_])(eprintln|println)\s*!").unwrap())
}

/// Names of raw print macros invoked on `line`, in order. Boundary-anchored so
/// `eprintln!` and `println!` are never conflated.
fn raw_print_macros(line: &str) -> Vec<String> {
    print_macro_regex()
        .captures_iter(line)
        .map(|c| c[1].to_string())
        .collect()
}

/// Remove `#[cfg(test)]`-guarded items so the guard only scans production code.
///
/// Handles the dominant `#[cfg(test)] mod tests { … }` pattern, inline
/// `#[cfg(test)] fn … { … }`, and brace-less `#[cfg(test)] mod tests;`
/// declarations. Brace-counted from the guarded item's first `{`; a `;` seen at
/// brace depth 0 before any `{` ends a brace-less declaration. Braces inside
/// string/char literals are a known limitation — they could only cause an
/// over-skip (never a false-negative on a production print), which is safe for
/// a guard whose failure mode we care about is missing a real offender.
fn strip_cfg_test(contents: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut lines = contents.lines();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            let mut depth: i32 = 0;
            let mut opened = false;
            'item: for body in lines.by_ref() {
                for ch in body.chars() {
                    match ch {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => {
                            depth -= 1;
                            if opened && depth <= 0 {
                                break 'item;
                            }
                        }
                        // Brace-less declaration (`mod tests;`): the item ends
                        // at the first top-level `;` before any block opens.
                        ';' if !opened => break 'item,
                        _ => {}
                    }
                }
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

#[test]
fn no_raw_print_macros_outside_output_module() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();
    scan_dir(&repo_root.join("src"), &repo_root, &mut offenders);

    assert!(
        offenders.is_empty(),
        "\nRaw `println!` / `eprintln!` found outside `src/output/` in a module \
         not on the allow-list.\n\
         Route human/operator/CLI output through `crate::output::ui::*` (channel \
         A/A2) or internal diagnostics through `tracing::{{debug,warn,…}}!` \
         (channel B). If a whole module has been migrated, remove it from \
         ALLOWLIST_SUFFIXES in this file.\n\n\
         Offenders:\n{}\n",
        offenders.join("\n")
    );
}

fn scan_dir(dir: &Path, repo_root: &Path, out: &mut Vec<String>) {
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
            scan_dir(&path, repo_root, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        // `src/output/` is the ui:: home; raw prints there are expected.
        if rel.starts_with(OUTPUT_HOME_PREFIX) {
            continue;
        }
        if ALLOWLIST_SUFFIXES.iter().any(|s| rel.ends_with(s)) {
            continue;
        }
        let contents = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let production = strip_cfg_test(&contents);
        for (lineno, line) in production.lines().enumerate() {
            for macro_name in raw_print_macros(line) {
                out.push(format!(
                    "  {rel}:{}: {macro_name}! — {}",
                    lineno + 1,
                    line.trim()
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eprintln_and_println_are_distinguished() {
        // The known-bad case: a substring search for "println!" would also count
        // "eprintln!". The boundary-anchored detector must keep them separate.
        assert_eq!(raw_print_macros(r#"    eprintln!("x");"#), vec!["eprintln"]);
        assert_eq!(raw_print_macros(r#"    println!("x");"#), vec!["println"]);

        // An eprintln! line is NOT mis-counted as a println! offender.
        let on_eprintln = raw_print_macros(r#"eprintln!("boom")"#);
        assert!(
            !on_eprintln.iter().any(|m| m == "println"),
            "eprintln! must not be counted as println!: {on_eprintln:?}"
        );
    }

    #[test]
    fn method_calls_named_like_macros_are_not_matched() {
        // `writeln!` and a `.println()` method are not raw print macros.
        assert!(raw_print_macros(r#"writeln!(out, "x")"#).is_empty());
        assert!(raw_print_macros("logger.println(x)").is_empty());
        // No `!`, so not a macro invocation.
        assert!(raw_print_macros("let println = 3;").is_empty());
    }

    #[test]
    fn strip_cfg_test_removes_test_module_body() {
        let src = "\
fn prod() { /* keep */ }
#[cfg(test)]
mod tests {
    fn helper() { /* dropped */ }
}
fn after() { /* keep */ }
";
        let stripped = strip_cfg_test(src);
        assert!(stripped.contains("fn prod()"));
        assert!(stripped.contains("fn after()"));
        assert!(
            !stripped.contains("fn helper()"),
            "test body must be stripped"
        );
    }

    #[test]
    fn strip_cfg_test_removes_inline_test_fn() {
        let src = "\
fn prod() {}
#[cfg(test)]
fn only_for_tests() { let x = 1; }
fn after() {}
";
        let stripped = strip_cfg_test(src);
        assert!(stripped.contains("fn prod()"));
        assert!(stripped.contains("fn after()"));
        assert!(!stripped.contains("only_for_tests"));
    }

    #[test]
    fn strip_cfg_test_handles_braceless_mod_decl() {
        let src = "\
fn prod() {}
#[cfg(test)]
mod tests;
fn after() {}
";
        let stripped = strip_cfg_test(src);
        // The brace-less decl line is consumed, production code on both sides
        // survives — the `;` must not eat the rest of the file.
        assert!(stripped.contains("fn prod()"));
        assert!(stripped.contains("fn after()"));
    }
}
