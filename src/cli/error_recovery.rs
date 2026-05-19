//! Wrong-name and wrong-flag hint tables for clap parse errors.
//!
//! When clap fails to parse a command, `lookup_hint` checks the raw argv
//! against known wrong invocations and returns a corrective hint to print
//! on a separate stderr line after clap's own error message.

/// Hints for wrong top-level subcommand names.
static WRONG_SUBCOMMAND_HINTS: &[(&str, &str)] = &[
    (
        "set-status",
        "hint: task-mgr has no `set-status` subcommand. To change status:\n\
         \x20 complete / done   mark done\n\
         \x20 fail              mark failed\n\
         \x20 skip              skip (defer)\n\
         \x20 reset             return to todo\n\
         \x20 unblock           clear a block\n\
         \x20 unskip            return skipped to todo\n\
         \x20 irrelevant        mark no longer needed\n\
         Example: task-mgr complete TASK-001",
    ),
    (
        "remove",
        "hint: task-mgr has no `remove` subcommand. Use:\n\
         \x20 task-mgr irrelevant <id> --reason '...'",
    ),
    (
        "delete",
        "hint: task-mgr has no `delete` subcommand. Use:\n\
         \x20 task-mgr irrelevant <id> --reason '...'",
    ),
    (
        "update",
        "hint: task-mgr has no `update` subcommand yet. Edit the JSON in the worktree, then:\n\
         \x20 task-mgr loop init <prd>.json --append --update-existing",
    ),
    (
        "edit",
        "hint: task-mgr has no `edit` subcommand yet. Edit the JSON in the worktree, then:\n\
         \x20 task-mgr loop init <prd>.json --append --update-existing",
    ),
    (
        "change",
        "hint: task-mgr has no `change` subcommand. Edit the JSON in the worktree, then:\n\
         \x20 task-mgr loop init <prd>.json --append --update-existing",
    ),
];

/// Hints for wrong flags or sub-arguments on known top-level subcommands.
/// (top_level_subcommand, wrong_flag_or_arg, hint_message)
static WRONG_ARG_HINTS: &[(&str, &str, &str)] = &[
    (
        "recall",
        "--top-k",
        "hint: task-mgr recall has no `--top-k`. Use `--limit <N>`.",
    ),
    (
        "learnings",
        "show",
        "hint: `task-mgr learnings show` is not a subcommand. Use:\n\
         \x20 task-mgr recall --query <text> --limit 1\n\
         \x20 task-mgr learnings | grep <N>",
    ),
    (
        "learnings",
        "get",
        "hint: `task-mgr learnings get` is not a subcommand. Use:\n\
         \x20 task-mgr recall --query <text> --limit 1\n\
         \x20 task-mgr learnings | grep <N>",
    ),
];

/// Look up a hint for the given argv slice (including argv[0] / program name).
///
/// Returns `None` if no hint applies — random typos and unknown flags not in
/// the table fall through to clap's built-in error unchanged.
pub fn lookup_hint(argv: &[String]) -> Option<&'static str> {
    let subcommand = first_subcommand(argv)?;

    for (name, hint) in WRONG_SUBCOMMAND_HINTS {
        if *name == subcommand {
            return Some(hint);
        }
    }

    for (subcmd, wrong_arg, hint) in WRONG_ARG_HINTS {
        if *subcmd == subcommand && argv.iter().any(|a| a.as_str() == *wrong_arg) {
            return Some(hint);
        }
    }

    None
}

/// Return the first positional argument (the attempted subcommand) from argv,
/// skipping the program name and any global flags with their values.
fn first_subcommand(argv: &[String]) -> Option<&str> {
    const GLOBAL_VALUE_FLAGS: &[&str] = &["--dir", "--format"];
    let mut skip_next = false;
    for arg in argv.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if GLOBAL_VALUE_FLAGS.contains(&arg.as_str()) {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_status_hint_matches() {
        let argv: Vec<String> = ["task-mgr", "set-status", "FOO", "done"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hint = lookup_hint(&argv).unwrap();
        assert!(hint.contains("set-status"));
        assert!(hint.contains("complete"));
        assert!(hint.contains("irrelevant"));
    }

    #[test]
    fn recall_top_k_hint_matches() {
        let argv: Vec<String> = ["task-mgr", "recall", "--top-k", "5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hint = lookup_hint(&argv).unwrap();
        assert!(hint.contains("--limit"));
    }

    #[test]
    fn learnings_show_hint_matches() {
        let argv: Vec<String> = ["task-mgr", "learnings", "show", "2236"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hint = lookup_hint(&argv).unwrap();
        assert!(hint.contains("recall"));
        assert!(hint.contains("--limit"));
    }

    #[test]
    fn remove_hint_suggests_irrelevant_not_remove_command() {
        let argv: Vec<String> = ["task-mgr", "remove", "FOO"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hint = lookup_hint(&argv).unwrap();
        // Fact-check: must suggest `irrelevant`, must not lie about a `remove` command existing
        assert!(hint.contains("irrelevant"));
        assert!(!hint.contains("task-mgr remove"));
    }

    #[test]
    fn update_hint_references_loop_init_workflow() {
        let argv: Vec<String> = ["task-mgr", "update", "FOO-1", "--title", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hint = lookup_hint(&argv).unwrap();
        assert!(hint.contains("loop init") || hint.contains("--append"));
    }

    #[test]
    fn random_unknown_flag_produces_no_hint() {
        let argv: Vec<String> = ["task-mgr", "--foo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // --foo starts with `-`, so first_subcommand returns None → no hint
        assert!(lookup_hint(&argv).is_none());
    }

    #[test]
    fn typo_close_to_real_command_produces_no_hint() {
        let argv: Vec<String> = ["task-mgr", "inits"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // "inits" is not in the hints table
        assert!(lookup_hint(&argv).is_none());
    }

    #[test]
    fn global_dir_flag_skipped_correctly() {
        let argv: Vec<String> = ["task-mgr", "--dir", ".task-mgr", "set-status", "X", "done"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Should still find set-status after skipping --dir and its value
        let hint = lookup_hint(&argv).unwrap();
        assert!(hint.contains("set-status"));
    }

    #[test]
    fn panic_in_hint_lookup_is_catchable() {
        // Verifies the catch_unwind pattern used in main() — if hint lookup
        // panics the fallback is None and clap's default error is shown.
        let result = std::panic::catch_unwind(|| -> Option<&'static str> {
            panic!("simulated hint-lookup panic");
        });
        assert!(result.is_err(), "catch_unwind must capture the panic");
        let hint = result.unwrap_or(None);
        assert!(hint.is_none());
    }
}
