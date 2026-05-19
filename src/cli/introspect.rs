//! Clap-introspection helper used by `task-mgr cheatsheet` and
//! `task-mgr enhance` to render a markdown command reference table from
//! the *compiled* CLI surface.
//!
//! Why a separate module: the same generator is consumed by
//!
//! - [`crate::commands::cheatsheet::cheatsheet`] (renders to stdout)
//! - [`crate::commands::enhance::templates::EnhanceProfile::body`] (renders
//!   into the marker-fenced block in CLAUDE.md / AGENTS.md)
//! - `tests/cheatsheet_drift.rs` (CI gate that ensures every non-hidden
//!   subcommand appears in the generator's output)
//!
//! Routing all three call sites through `generate_command_reference` makes
//! "the docs cannot lie about command names" a single-source-of-truth
//! property — the generator literally walks
//! `clap::Command::get_subcommands()` from [`crate::cli::Cli::command`],
//! so any subcommand added to the clap derive shows up automatically.
//!
//! Per the FEAT-002 acceptance criteria, this module MUST NOT spawn
//! `task-mgr --help` as a subprocess. We rely on `clap::CommandFactory`
//! exclusively.

use clap::CommandFactory;

use super::Cli;

/// Maximum chars of an `about` line we'll splice into a single table row.
/// Long descriptions are truncated with a trailing ellipsis so the table
/// stays readable in 80-col terminals.
const MAX_ABOUT_CHARS: usize = 80;

/// Render the canonical markdown table of every non-hidden subcommand
/// (and nested subcommand) of [`Cli`].
///
/// Top-level entry point — callers in cheatsheet / enhance / drift tests
/// all consume this exact function so the table never diverges from clap.
///
/// Returns an empty string when clap reports zero subcommands (a
/// degenerate test state). A stderr warning is emitted so the operator
/// sees the renderer falling back.
pub fn generate_command_reference() -> String {
    let cmd = Cli::command();
    generate_command_reference_from(&cmd)
}

/// Pure-input version used by unit tests that want to verify behavior on
/// a synthetic `clap::Command` (e.g. the zero-subcommand failure mode).
///
/// Walks `cmd.get_subcommands()` and any one level of nesting (sufficient
/// for `loop init` / `batch run` / `run begin` etc.). Hidden subcommands
/// are excluded.
pub fn generate_command_reference_from(cmd: &clap::Command) -> String {
    let entries = collect_subcommand_entries(cmd);

    if entries.is_empty() {
        eprintln!(
            "warning: task-mgr cheatsheet: clap reported zero subcommands; \
             rendering curated recipes only"
        );
        return String::new();
    }

    let mut out = String::with_capacity(64 + entries.len() * 80);
    out.push_str("| Command | Description |\n");
    out.push_str("| --- | --- |\n");
    for (path, about) in entries {
        out.push_str("| `task-mgr ");
        out.push_str(&path);
        out.push_str("` | ");
        out.push_str(&about);
        out.push_str(" |\n");
    }
    out
}

/// Walk the command tree and collect `(path, about)` pairs for the
/// reference table. Single-level nesting only; that's all task-mgr's CLI
/// uses today (`loop init`, `batch run`, `run begin`, …) and adding
/// deeper recursion would inflate the table for no current consumer.
fn collect_subcommand_entries(cmd: &clap::Command) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for sub in cmd.get_subcommands() {
        if sub.is_hide_set() {
            continue;
        }
        let name = sub.get_name().to_string();
        let about = first_line_truncated(
            sub.get_about()
                .map(|s| s.to_string())
                .unwrap_or_default()
                .as_str(),
        );
        entries.push((name.clone(), about));

        for nested in sub.get_subcommands() {
            if nested.is_hide_set() {
                continue;
            }
            let nested_path = format!("{} {}", name, nested.get_name());
            let nested_about = first_line_truncated(
                nested
                    .get_about()
                    .map(|s| s.to_string())
                    .unwrap_or_default()
                    .as_str(),
            );
            entries.push((nested_path, nested_about));
        }
    }
    entries
}

/// Take the first non-empty line of `s` and truncate it to
/// `MAX_ABOUT_CHARS` Unicode characters, appending an ellipsis when
/// truncated. Char-counted (not byte-counted) so we never split a
/// multi-byte codepoint.
fn first_line_truncated(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > MAX_ABOUT_CHARS {
        let truncated: String = chars
            .iter()
            .take(MAX_ABOUT_CHARS.saturating_sub(1))
            .collect();
        format!("{}…", truncated)
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, Command};

    #[test]
    fn renders_table_header_when_subcommands_present() {
        let out = generate_command_reference();
        assert!(out.starts_with("| Command | Description |\n"));
        assert!(out.contains("| --- | --- |\n"));
    }

    #[test]
    fn renders_top_level_subcommand_rows_with_task_mgr_prefix() {
        let out = generate_command_reference();
        // Pick a few stable subcommands that have existed for many releases.
        for name in &["init", "list", "show", "next", "complete", "current"] {
            let token = format!("| `task-mgr {}` |", name);
            assert!(out.contains(&token), "missing row for `task-mgr {}`", name);
        }
    }

    #[test]
    fn renders_nested_loop_init_and_loop_run() {
        let out = generate_command_reference();
        assert!(out.contains("| `task-mgr loop init` |"));
        assert!(out.contains("| `task-mgr loop run` |"));
        assert!(out.contains("| `task-mgr batch init` |"));
        assert!(out.contains("| `task-mgr batch run` |"));
    }

    #[test]
    fn excludes_hidden_subcommands() {
        let cmd = Command::new("root")
            .subcommand(Command::new("visible").about("visible cmd"))
            .subcommand(Command::new("hidden").about("hidden cmd").hide(true));
        let out = generate_command_reference_from(&cmd);
        assert!(out.contains("`task-mgr visible`"));
        assert!(!out.contains("hidden"));
    }

    #[test]
    fn zero_subcommands_returns_empty_string_no_panic() {
        // FEAT-002 failure mode: CommandFactory returns zero subcommands
        // → cheatsheet renders curated section only with a stderr warning,
        // no panic.
        let cmd = Command::new("empty");
        let out = generate_command_reference_from(&cmd);
        assert!(out.is_empty(), "expected empty string, got: {out:?}");
    }

    #[test]
    fn first_line_truncated_takes_only_first_line() {
        let s = "first line\nsecond line\nthird";
        assert_eq!(first_line_truncated(s), "first line");
    }

    #[test]
    fn first_line_truncated_handles_empty() {
        assert_eq!(first_line_truncated(""), "");
        assert_eq!(first_line_truncated("\n\n"), "");
    }

    #[test]
    fn first_line_truncated_ellipsizes_long_lines() {
        let s = "a".repeat(120);
        let out = first_line_truncated(&s);
        assert!(out.ends_with('…'));
        let char_count = out.chars().count();
        assert_eq!(char_count, MAX_ABOUT_CHARS);
    }

    #[test]
    fn first_line_truncated_preserves_multibyte_codepoints() {
        // 100 copies of a 3-byte UTF-8 codepoint.
        let s = "中".repeat(100);
        let out = first_line_truncated(&s);
        // Must not panic, must end with ellipsis, char count == MAX.
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_ABOUT_CHARS);
    }

    #[test]
    fn synthetic_command_with_arg_does_not_appear_as_subcommand() {
        let cmd = Command::new("root").subcommand(
            Command::new("sub")
                .about("only one sub")
                .arg(Arg::new("flag").long("flag")),
        );
        let out = generate_command_reference_from(&cmd);
        assert!(out.contains("`task-mgr sub`"));
        // The --flag is not a subcommand; never appears as a row.
        assert!(!out.lines().any(|l| l.contains("`task-mgr flag`")));
    }
}
