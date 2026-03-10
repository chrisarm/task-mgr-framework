pub mod archive;
pub mod archive_display;
pub mod batch;
pub mod branch;
pub mod calibrate;
pub mod calibrate_math;
pub mod claude;
pub mod config;
pub mod context;
pub mod crash;
pub mod deadline;
pub mod detection;
pub mod display;
pub mod engine;
pub mod env;
pub mod feedback;
pub mod git_reconcile;
pub mod guidance;
pub mod model;
pub mod monitor;
pub mod oauth;
pub mod output_parsing;
pub mod prd_reconcile;
pub mod progress;
pub(crate) mod project_config;
pub mod prompt;
pub mod prompt_sections;
pub mod signals;
pub mod stale;
pub mod status;
pub mod status_display;
pub mod status_queries;
pub mod usage;
pub mod watchdog;
pub mod worktree;

#[cfg(test)]
pub(crate) mod test_utils;

// Signal and state file name constants used across loop_engine modules.
// Centralised here to prevent typo bugs and ensure consistent naming.

/// File name for the stop signal (created by user to request loop termination).
pub(crate) const STOP_FILE: &str = ".stop";

/// File name for the pause signal (created by user to pause and provide guidance).
pub(crate) const PAUSE_FILE: &str = ".pause";

/// Prefix for deadline files (format: `.deadline-<prd-basename>`).
pub(crate) const DEADLINE_FILE_PREFIX: &str = ".deadline-";

/// File name for tracking the last active branch.
pub(crate) const LAST_BRANCH_FILE: &str = ".last-branch";

/// Sanitize error messages to prevent token/credential leakage.
///
/// Removes any potential token values (long alphanumeric strings >20 chars)
/// from error strings, replacing them with `[REDACTED]`.
pub(crate) fn sanitize_error_tokens(error: &str) -> String {
    error
        .split_whitespace()
        .map(|word| {
            if word.len() > 20
                && word
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                "[REDACTED]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
