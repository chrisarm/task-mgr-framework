//! Grep regression guard (codex-runner FEAT-001): production code MUST NOT
//! call [`task_mgr::loop_engine::engine::resolve_effective_runner`] with a
//! bare `Option<&str>` third argument.
//!
//! The convenience `impl From<Option<&str>> for EffectiveRunnerInput` is
//! gated `#[cfg(test)]` so unit tests inside the library crate can stay
//! terse, but every production call site must construct an
//! `EffectiveRunnerInput { model, provider_hint }` explicitly. A bare-Option
//! production call would silently set `provider_hint: None` and route a
//! Codex slot's `gpt-*`/`o*`/`codex` model id back to Claude — exactly the
//! known-bad regression the threading defense exists to prevent.
//!
//! ## Why this scanner is multi-line-aware
//!
//! Every real production call site spans several lines:
//!
//! ```text
//! let effective_runner = resolve_effective_runner(
//!     ctx,
//!     &task_id,
//!     EffectiveRunnerInput { model, provider_hint },
//! );
//! ```
//!
//! A naive single-line regex (`resolve_effective_runner\s*\(...\)` anchored to
//! one line) matches NONE of these — the opening line has no closing `)`. That
//! makes the guard vacuously pass: it neither flags a bad call nor confirms a
//! good one. So this scanner does balanced-paren extraction across newlines and
//! additionally asserts a positive floor on the number of legitimate production
//! call sites it actually inspected (`MIN_PRODUCTION_CALL_SITES`). If a refactor
//! hides every call from the scanner, the floor assertion fails loudly instead
//! of the guard going silently blind.
//!
//! The scan is restricted to `src/` (production code only); `tests/` and
//! `#[cfg(test)]` library modules are allowed to use either form.

use std::fs;
use std::path::{Path, PathBuf};

/// File extensions to scan for production call sites.
const SCAN_EXTENSIONS: &[&str] = &["rs"];

/// Skip target/build artifacts.
const SKIP_DIR_SUFFIXES: &[&str] = &["target", ".git"];

/// The known production spawn-discriminant call sites: sequential spawn
/// (`iteration.rs`), wave pre-spawn (`wave_scheduler.rs`), the reactions
/// pre-spawn hook (`reactions/pre_spawn.rs`), the slot merge re-derivation
/// assertion (`slot.rs`), and the recovery inner derivation (`recovery.rs`).
/// The scanner MUST see at least this many legitimate calls; a lower count
/// means a call site was moved out of view (or the scanner regressed to
/// single-line blindness) and the guard is no longer protecting it. If you
/// intentionally add/remove a spawn site, update this floor and the comment.
const MIN_PRODUCTION_CALL_SITES: usize = 4;

#[derive(Debug)]
struct CallSite {
    location: String,
    /// Full argument text between the outermost parens (may span lines).
    args: String,
}

#[test]
fn no_bare_option_call_to_resolve_effective_runner_in_production() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_dir = repo_root.join("src");

    let mut calls: Vec<CallSite> = Vec::new();
    scan_dir(&src_dir, &repo_root, &mut calls);

    // A legitimate call constructs `EffectiveRunnerInput { ... }` inline, or
    // passes a pre-built input (`input`) / the test-only `.into()` shortcut.
    // A bare-Option offender's last argument is a `Some(...)` / `None` /
    // `.as_deref()` expression with neither marker.
    let mut offenders: Vec<String> = Vec::new();
    let mut legitimate = 0usize;
    for call in &calls {
        if call_is_legitimate(&call.args) {
            legitimate += 1;
        } else {
            offenders.push(format!(
                "  {}: args=`{}`",
                call.location,
                squish(&call.args)
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "\nBare-Option call to `resolve_effective_runner(_, _, <Option>)` found in production code.\n\
         Use the explicit `EffectiveRunnerInput {{ model, provider_hint }}` form instead — the bare-\n\
         Option `From<Option<&str>>` conversion is `#[cfg(test)]`-gated to prevent provider intent from\n\
         being silently dropped. See codex-runner FEAT-001.\n\n\
         Offenders:\n{}\n",
        offenders.join("\n")
    );

    // Positive floor: prove the scanner actually parsed the multi-line
    // production call sites. Without this, a scanner that matches nothing
    // (the pre-FEAT-007 single-line-regex bug) would pass vacuously.
    assert!(
        legitimate >= MIN_PRODUCTION_CALL_SITES,
        "\nExpected to inspect at least {MIN_PRODUCTION_CALL_SITES} legitimate production call site(s) of \
         `resolve_effective_runner`, but only found {legitimate}.\n\
         Either a spawn-discriminant call site was moved out of `src/`, or the scanner regressed to \
         single-line blindness (the original Low-2 bug). Sites seen:\n{}\n",
        calls
            .iter()
            .map(|c| format!("  {}", c.location))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// `EffectiveRunnerInput { ... }` (inline struct), a bare `input` identifier
/// (a pre-built `EffectiveRunnerInput` threaded in), or the `#[cfg(test)]`
/// `.into()` shortcut are the only legitimate shapes. Anything else with a
/// `Some(`/`None`/`.as_deref()` tail is a bare-Option offender.
fn call_is_legitimate(args: &str) -> bool {
    let last = last_top_level_arg(args);
    last.contains("EffectiveRunnerInput") || last.ends_with(".into()") || last == "input"
}

/// Split the call's argument blob on top-level commas (ignoring commas nested
/// inside `()`, `[]`, or `{}`) and return the trimmed final NON-EMPTY argument.
/// Skipping empties tolerates a trailing comma after the last argument
/// (`EffectiveRunnerInput { .. },`) — common rustfmt output that would
/// otherwise leave an empty final segment.
fn last_top_level_arg(args: &str) -> String {
    let mut depth = 0i32;
    let mut segments: Vec<&str> = Vec::new();
    let mut seg_start = 0usize;
    for (i, ch) in args.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                segments.push(&args[seg_start..i]);
                seg_start = i + 1;
            }
            _ => {}
        }
    }
    segments.push(&args[seg_start..]);
    segments
        .iter()
        .rev()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

fn scan_dir(dir: &Path, repo_root: &Path, out: &mut Vec<CallSite>) {
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
        if !has_scanned_extension(&path) {
            continue;
        }
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let contents = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let production = strip_test_and_comments(&contents);
        find_calls(&rel, &production, out);
    }
}

fn has_scanned_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SCAN_EXTENSIONS.contains(&e))
        .unwrap_or(false)
}

/// Produce a line-count-preserving copy of the file with (a) every line inside
/// a `#[cfg(test)] mod ... { ... }` block blanked and (b) line-comment tails
/// (`// ...`) stripped. Blanking rather than deleting keeps reported line
/// numbers aligned with the original file. This is the call-site filter: only
/// production code outside cfg(test) survives, and comment mentions of
/// `resolve_effective_runner` can't masquerade as calls.
fn strip_test_and_comments(contents: &str) -> String {
    let mut out = String::with_capacity(contents.len());
    let mut depth = 0i32; // brace depth inside an open cfg(test) module
    let mut pending_cfg_test = false;

    for line in contents.lines() {
        let trimmed = line.trim_start();

        // Open the test region on the `mod ... {` line following `#[cfg(test)]`.
        if pending_cfg_test && trimmed.contains("mod ") && line.contains('{') {
            depth += 1;
            pending_cfg_test = false;
            out.push('\n');
            continue;
        }
        if pending_cfg_test {
            // Allow further attribute lines between `#[cfg(test)]` and `mod`.
            if trimmed.starts_with("#[") || trimmed.is_empty() {
                out.push('\n');
                continue;
            }
            // `#[cfg(test)]` decorated a fn/use/item that isn't a mod — the
            // following items are NOT a test module; stop pending and let the
            // line fall through to normal handling.
            pending_cfg_test = false;
        }
        if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
            out.push('\n');
            continue;
        }
        if depth > 0 {
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if depth < 0 {
                depth = 0;
            }
            out.push('\n'); // blank the test-module line
            continue;
        }

        // Strip a line-comment tail so `// resolve_effective_runner(...)` notes
        // are not mistaken for calls. No string literal in this codebase
        // contains `//` before a `resolve_effective_runner` token, so a plain
        // first-`//` truncation is safe for this guard.
        let code = match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        };
        out.push_str(code);
        out.push('\n');
    }
    out
}

/// Find every `resolve_effective_runner(...)` call in production code via
/// balanced-paren extraction (multi-line aware), skipping the `fn` definition.
fn find_calls(rel: &str, code: &str, out: &mut Vec<CallSite>) {
    const NEEDLE: &str = "resolve_effective_runner";
    let bytes = code.as_bytes();
    let mut search_from = 0usize;

    while let Some(rel_idx) = code[search_from..].find(NEEDLE) {
        let idx = search_from + rel_idx;
        let after = idx + NEEDLE.len();
        search_from = after;

        // Skip the function definition: `fn resolve_effective_runner`.
        let prefix = code[..idx].trim_end();
        if prefix.ends_with("fn") {
            continue;
        }
        // Skip non-call references (e.g. `Self::resolve_effective_runner` used
        // as a value, or a longer identifier): the next non-ws char must be `(`.
        let mut j = after;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            continue;
        }

        // Balanced-paren capture of the argument list.
        let args_start = j + 1;
        let mut depth = 1i32;
        let mut k = args_start;
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            k += 1;
        }
        if depth != 0 {
            continue; // unbalanced (truncated file) — ignore
        }
        let args = &code[args_start..k - 1];
        let line_no = code[..idx].bytes().filter(|&b| b == b'\n').count() + 1;
        out.push(CallSite {
            location: format!("{rel}:{line_no}"),
            args: args.to_string(),
        });
        search_from = k;
    }
}

/// Collapse whitespace/newlines in a captured arg blob for one-line reporting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Self-tests for the scanner itself. These prove the multi-line machinery
// catches what the original single-line regex could not — without needing a
// non-compiling production mutation (the bare-Option form won't compile in
// production because `From<Option<&str>>` is `#[cfg(test)]`-gated, so the type
// system is the primary guard and these tests cover the belt-and-suspenders
// scanner directly).

/// Helper: run the scanner over an in-memory snippet and return call sites.
fn scan_snippet(code: &str) -> Vec<CallSite> {
    let production = strip_test_and_comments(code);
    let mut calls = Vec::new();
    find_calls("snippet.rs", &production, &mut calls);
    calls
}

#[test]
fn detects_multiline_bare_option_offender() {
    // The exact shape the old single-line regex was blind to: the call opens
    // on one line and the bare-Option arg lands on later lines.
    let code = "\
fn spawn() {
    let r = resolve_effective_runner(
        ctx,
        &task_id,
        effective_model.as_deref(),
    );
}
";
    let calls = scan_snippet(code);
    assert_eq!(calls.len(), 1, "scanner must see the multi-line call");
    assert!(
        !call_is_legitimate(&calls[0].args),
        "a bare `.as_deref()` last arg must be flagged as an offender, got: {}",
        calls[0].args
    );
}

#[test]
fn accepts_multiline_struct_with_trailing_comma() {
    // The real production shape — inline struct, rustfmt trailing comma.
    let code = "\
fn spawn() {
    let r = resolve_effective_runner(
        ctx,
        &task_id,
        EffectiveRunnerInput { model: m, provider_hint: h },
    );
}
";
    let calls = scan_snippet(code);
    assert_eq!(calls.len(), 1);
    assert!(
        call_is_legitimate(&calls[0].args),
        "inline EffectiveRunnerInput with a trailing comma must be legitimate, got: {}",
        calls[0].args
    );
}

#[test]
fn accepts_prebuilt_input_and_into_shortcut() {
    let code = "\
fn a() { let _ = resolve_effective_runner(ctx, id, input); }
fn b() { let _ = resolve_effective_runner(ctx, id, Some(m).into()); }
";
    let calls = scan_snippet(code);
    assert_eq!(calls.len(), 2);
    assert!(call_is_legitimate(&calls[0].args), "bare `input` is legit");
    assert!(call_is_legitimate(&calls[1].args), "`.into()` is legit");
}

#[test]
fn skips_cfg_test_calls_and_comments_and_definition() {
    let code = "\
// resolve_effective_runner(ctx, id, None) -- a comment, not a call
pub fn resolve_effective_runner(a: A, b: B, c: C) -> RunnerKind { todo!() }

fn real() { let _ = resolve_effective_runner(ctx, id, input); }

#[cfg(test)]
mod tests {
    fn t() {
        let _ = resolve_effective_runner(ctx, id, None);
        let _ = resolve_effective_runner(ctx, id, Some(\"gpt-5\"));
    }
}
";
    let calls = scan_snippet(code);
    // Only the one real production call survives: the comment, the `fn`
    // definition, and both cfg(test) calls are filtered out.
    assert_eq!(
        calls.len(),
        1,
        "expected exactly the production call, got: {:?}",
        calls.iter().map(|c| &c.location).collect::<Vec<_>>()
    );
    assert!(call_is_legitimate(&calls[0].args));
}
