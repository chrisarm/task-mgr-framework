//! Drift CI for the `task-mgr cheatsheet` command.
//!
//! Three orthogonal checks run together as the FEAT-002 gate:
//!
//! 1. **Subcommand drift** — every non-hidden subcommand returned by
//!    `clap::CommandFactory::command().get_subcommands()` must appear in
//!    the rendered reference table. Catches "new subcommand added to
//!    clap but the docs don't mention it".
//!
//! 2. **Curated-recipe subcommand drift** — every `task-mgr <subcommand>`
//!    token in the curated recipes block must resolve to a real clap
//!    subcommand. Catches "docs reference a command that doesn't exist"
//!    (the historical `set-status` bug).
//!
//! 3. **Curated-recipe flag drift** — every `--<flag>` mentioned in the
//!    curated recipes (scoped to a backtick-delimited code span around a
//!    `task-mgr <cmd>` invocation) must exist on that subcommand. Catches
//!    "docs reference a flag the binary doesn't accept" (e.g.
//!    `recall --top-k`).
//!
//! Hermetic: runs entirely against `task_mgr::cli::Cli::command()` and
//! `task_mgr::commands::cheatsheet::cheatsheet()`. No fs access, no
//! subprocess. The full file should execute in well under 100 ms.
//!
//! Per the FEAT-002 acceptance criteria, this file MUST NOT spawn the
//! `task-mgr` binary via assert_cmd — we never invoke a subprocess; we
//! read the clap tree directly.

use clap::CommandFactory;
use task_mgr::cli::Cli;
use task_mgr::cli::introspect::generate_command_reference;
use task_mgr::commands::cheatsheet::{CURATED_RECIPES, cheatsheet};

// ─── helpers ───────────────────────────────────────────────────────────────

/// One `task-mgr <subcommand>` call extracted from the curated recipes
/// block. `path` is space-separated (e.g. "loop init"); `flags` is every
/// `--<long>` form found within the same backtick-delimited code span.
#[derive(Debug, Clone)]
struct RecipeCall {
    path: String,
    flags: Vec<String>,
}

/// Pull every `task-mgr <cmd>` invocation out of the curated recipes
/// block, along with the flags that appear after it in the same backtick
/// span.
///
/// Why backtick-scoped: the recipes use prose like `task-mgr complete
/// <id>` (also: `task-mgr fail`, ...). Without scoping, flags from a
/// LATER backtick span would get misattributed to the FIRST command on
/// the line. Backticks delimit "this is one invocation".
fn extract_recipe_calls(content: &str) -> Vec<RecipeCall> {
    let mut out = Vec::new();
    let mut chars = content.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if c != '`' {
            continue;
        }
        // Find the matching closing backtick on the same line (the
        // recipe block uses single-line code spans only — no triple
        // backticks, no embedded newlines).
        let span_start = idx + 1;
        let mut span_end = None;
        for (j, k) in content[span_start..].char_indices() {
            if k == '\n' {
                break;
            }
            if k == '`' {
                span_end = Some(span_start + j);
                break;
            }
        }
        let Some(end) = span_end else { continue };
        let span = &content[span_start..end];

        // Advance the outer iterator past this span so we don't re-enter.
        while let Some(&(j, _)) = chars.peek() {
            if j > end {
                break;
            }
            chars.next();
        }

        // Find every `task-mgr <subcommand-path>` start within the span.
        // Two-level paths (`loop init`, `batch run`, etc.) are recognised
        // when the second token is the well-known nested name.
        const TWO_LEVEL_NESTED: &[(&str, &[&str])] = &[
            ("loop", &["init", "run"]),
            ("batch", &["init", "run"]),
            ("decisions", &["list", "resolve", "decline", "revert"]),
            ("models", &["list", "set-default", "unset-default", "show"]),
            (
                "curate",
                &["retire", "unretire", "dedup", "enrich", "embed", "count"],
            ),
            ("worktrees", &["list", "prune", "remove"]),
            ("migrate", &["status", "up", "down", "all"]),
            ("enhance", &["agents", "show", "strip"]),
            ("run", &["begin", "update", "end"]),
        ];
        let mut search_from = 0;
        while let Some(off) = span[search_from..].find("task-mgr ") {
            let cmd_start = search_from + off + "task-mgr ".len();
            // Pull the first identifier-ish token.
            let first = take_ident(&span[cmd_start..]);
            if first.is_empty() {
                search_from = cmd_start;
                continue;
            }
            let after_first = cmd_start + first.len();
            // Try to extend to a two-level path.
            let mut path = first.to_string();
            let mut after_path = after_first;
            for (parent, children) in TWO_LEVEL_NESTED {
                if *parent == first {
                    let rest = &span[after_first..];
                    let rest_trimmed = rest.trim_start();
                    let consumed_ws = rest.len() - rest_trimmed.len();
                    let second = take_ident(rest_trimmed);
                    if !second.is_empty() && children.contains(&second) {
                        path.push(' ');
                        path.push_str(second);
                        after_path = after_first + consumed_ws + second.len();
                    }
                    break;
                }
            }

            // Flags: every "--<long>" between after_path and the END of
            // THIS task-mgr invocation. The invocation ends either at
            // the next "task-mgr " token in the same span or at the end
            // of the span.
            let next_start = span[after_path..]
                .find("task-mgr ")
                .map(|i| after_path + i)
                .unwrap_or(span.len());
            let scope = &span[after_path..next_start];
            let flags = extract_long_flags(scope);

            out.push(RecipeCall { path, flags });
            search_from = next_start;
        }
    }
    out
}

/// Take the leading `[a-z][a-z0-9-]*` ident from `s`.
fn take_ident(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return "";
    }
    let mut end = 1;
    while end < bytes.len() {
        let b = bytes[end];
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' {
            end += 1;
        } else {
            break;
        }
    }
    &s[..end]
}

/// Every `--<long>` flag long-name in `s` (no leading hyphens; alpha-only
/// start, then alphanumeric or hyphen).
fn extract_long_flags(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_alphabetic() {
            let start = i + 2;
            let mut end = start + 1;
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_alphanumeric() || b == b'-' {
                    end += 1;
                } else {
                    break;
                }
            }
            out.push(s[start..end].to_string());
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Walk the clap tree by `path` (space-separated, max 2 levels). Returns
/// `Some(&Command)` when the full path resolves; `None` otherwise.
fn find_command<'a>(root: &'a clap::Command, path: &str) -> Option<&'a clap::Command> {
    let mut current = root;
    for part in path.split_whitespace() {
        current = current.find_subcommand(part)?;
    }
    Some(current)
}

/// Whether `cmd` (or `root` for global flags) accepts the `--<flag>` long
/// option. Globals are honored because operators pass things like
/// `--format json` to any subcommand.
fn has_long_flag(root: &clap::Command, cmd: &clap::Command, flag: &str) -> bool {
    cmd.get_arguments().any(|a| a.get_long() == Some(flag))
        || root
            .get_arguments()
            .any(|a| a.get_long() == Some(flag) && a.is_global_set())
}

// ─── tests ─────────────────────────────────────────────────────────────────

#[test]
fn drift_every_visible_subcommand_appears_in_generated_reference() {
    // AC: "Drift test: tests/cheatsheet_drift.rs enumerates
    // clap::CommandFactory::command().get_subcommands() and asserts every
    // non-hidden subcommand appears in generate_command_reference()
    // output".
    let cmd = Cli::command();
    let reference = generate_command_reference();
    let mut missing = Vec::new();
    for sub in cmd.get_subcommands() {
        if sub.is_hide_set() {
            continue;
        }
        let token = format!("| `task-mgr {}` |", sub.get_name());
        if !reference.contains(&token) {
            missing.push(sub.get_name().to_string());
        }
        for nested in sub.get_subcommands() {
            if nested.is_hide_set() {
                continue;
            }
            let nested_token = format!("| `task-mgr {} {}` |", sub.get_name(), nested.get_name());
            if !reference.contains(&nested_token) {
                missing.push(format!("{} {}", sub.get_name(), nested.get_name()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "generate_command_reference() is missing rows for: {missing:?}\n\
         Add the subcommand to the clap derive OR explicitly hide it."
    );
}

#[test]
fn drift_cheatsheet_reference_table_matches_enhance_show_table() {
    // AC: "Positive: `task-mgr enhance show` output contains the SAME
    // generated reference table as `task-mgr cheatsheet` (substring
    // match)".
    use task_mgr::commands::enhance::templates::EnhanceProfile;
    use task_mgr::commands::enhance::{ShowParams, enhance_show};

    let cheatsheet_content = cheatsheet().content;
    let show = enhance_show(ShowParams {
        profile: EnhanceProfile::Workflow,
    })
    .expect("enhance show must succeed");
    let show_body = show.rendered.expect("show must populate rendered");

    let reference = generate_command_reference();
    assert!(!reference.is_empty(), "reference table must not be empty");
    assert!(
        cheatsheet_content.contains(&reference),
        "cheatsheet must embed the generated reference table verbatim"
    );
    assert!(
        show_body.contains(&reference),
        "enhance show body must embed the generated reference table verbatim"
    );
}

#[test]
fn drift_curated_recipe_subcommands_resolve_against_clap() {
    // Known-bad scenario: introducing a `set-status` reference into the
    // curated recipes block (without adding a real `set-status`
    // subcommand) must cause this test to fail with a clear diff.
    let cmd = Cli::command();
    let calls = extract_recipe_calls(CURATED_RECIPES);
    assert!(
        !calls.is_empty(),
        "extract_recipe_calls returned 0 calls — recipe parser may be broken"
    );

    let mut bad: Vec<String> = Vec::new();
    for call in &calls {
        if find_command(&cmd, &call.path).is_none() {
            bad.push(call.path.clone());
        }
    }
    bad.sort();
    bad.dedup();
    assert!(
        bad.is_empty(),
        "curated recipes reference {bad:?} but those subcommands don't exist in clap. \
         Either add them to the clap derive or remove the mention from CURATED_RECIPES."
    );
}

#[test]
fn drift_curated_recipe_flags_exist_on_their_subcommand() {
    // AC: "Drift test: for every `--flag` mentioned in the curated
    // recipes block, asserts the flag exists on its named subcommand
    // (parses the recipe markdown for `task-mgr <cmd> ... --<flag>`
    // patterns)".
    let cmd = Cli::command();
    let calls = extract_recipe_calls(CURATED_RECIPES);

    let mut violations: Vec<String> = Vec::new();
    for call in &calls {
        let Some(sub) = find_command(&cmd, &call.path) else {
            // Unknown-subcommand case is covered by the other test;
            // don't double-report.
            continue;
        };
        for flag in &call.flags {
            if !has_long_flag(&cmd, sub, flag) {
                violations.push(format!("--{flag} on `task-mgr {}`", call.path));
            }
        }
    }
    violations.sort();
    violations.dedup();
    assert!(
        violations.is_empty(),
        "curated recipes reference these flags that clap doesn't accept:\n  {}\n\
         Either add the flag to the clap derive or remove the mention from CURATED_RECIPES.",
        violations.join("\n  ")
    );
}

#[test]
fn drift_synthetic_set_status_reference_is_rejected() {
    // AC: "Drift test: introducing a `set-status` reference into the
    // curated recipes block (without adding a real `set-status`
    // subcommand) causes the test to fail with a clear diff".
    //
    // Verifies the drift-detection mechanism itself: we feed a synthetic
    // recipe that mentions `task-mgr set-status` and assert that the
    // unknown-subcommand check would catch it. If this test breaks, the
    // real curated-recipes check above can no longer be trusted.
    let synthetic = "- Mark status (outside loop): `task-mgr set-status <id> done`\n";
    let calls = extract_recipe_calls(synthetic);
    assert!(
        calls.iter().any(|c| c.path == "set-status"),
        "parser must pick up the synthetic set-status reference"
    );
    let cmd = Cli::command();
    assert!(
        find_command(&cmd, "set-status").is_none(),
        "if `task-mgr set-status` ever becomes a real subcommand, update this test \
         (the synthetic anchor needs to switch to a new known-bad name)"
    );
}

#[test]
fn drift_cheatsheet_stdout_never_contains_forbidden_anchors() {
    // AC: "Negative: the strings `set-status`, `add --from-json`,
    // `recall --top-k`, `learnings show <N>` MUST NOT appear in
    // `task-mgr cheatsheet` stdout (assert with grep)".
    let stdout = cheatsheet().content;
    for anchor in [
        "set-status",
        "add --from-json",
        "recall --top-k",
        "learnings show ",
    ] {
        assert!(
            !stdout.contains(anchor),
            "cheatsheet stdout contains forbidden anchor {anchor:?}; full content:\n{stdout}"
        );
    }
}

#[test]
fn drift_curated_recipes_under_thirty_lines() {
    // qualityDimensions: "curated recipes <= 30 lines".
    let line_count = CURATED_RECIPES.lines().count();
    assert!(
        line_count <= 30,
        "curated recipes must be <= 30 lines, was {line_count}"
    );
}

#[test]
fn drift_recipe_call_extractor_handles_basic_shapes() {
    // Belt-and-braces unit test for the extractor itself so a future
    // contributor can't silently break the parser and have the drift
    // tests pass on zero calls.
    let calls = extract_recipe_calls(
        "- foo: `task-mgr next --claim --prefix P`\n\
         - bar: `task-mgr loop init <prd>.json --append --update-existing`\n\
         - baz: `task-mgr show <id>` (also: `task-mgr complete <id>`)\n",
    );
    let paths: Vec<&str> = calls.iter().map(|c| c.path.as_str()).collect();
    assert!(paths.contains(&"next"));
    assert!(paths.contains(&"loop init"));
    assert!(paths.contains(&"show"));
    assert!(paths.contains(&"complete"));

    let next = calls.iter().find(|c| c.path == "next").unwrap();
    assert!(next.flags.contains(&"claim".to_string()));
    assert!(next.flags.contains(&"prefix".to_string()));

    let loop_init = calls.iter().find(|c| c.path == "loop init").unwrap();
    assert!(loop_init.flags.contains(&"append".to_string()));
    assert!(loop_init.flags.contains(&"update-existing".to_string()));
}
