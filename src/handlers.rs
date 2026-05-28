//! Output handlers and helper functions for CLI commands.
//!
//! This module contains:
//! - `TextFormattable` trait for unified text output formatting
//! - Generic `output_result` function replacing per-type output functions
//! - Helper functions for type conversions
//! - Man page and shell completion generation utilities
//!
//! Extracted from main.rs to keep the main module focused on
//! argument parsing and command dispatch.

use std::fs;
use std::io;
use std::path::Path;
use std::process;

use clap::CommandFactory;
use clap_complete::{Shell as CompletionShell, generate};
use clap_mangen::Man;

use crate::TaskMgrError;
use crate::cli::{Cli, OutputFormat, Shell};
use crate::commands::curate::{
    CountResult, DedupResult, EmbedResult, EnrichResult, RetireResult, UnretireResult,
    format_count_text, format_dedup_text, format_embed_text, format_enrich_text,
    format_retire_text, format_unretire_text,
};
use crate::commands::{
    ApplyLearningResult, BeginResult, CheatsheetResult, CompleteResult, CurrentResult,
    DecisionDeclineResult, DecisionResolveResult, DecisionRevertResult, DecisionsListResult,
    DoctorResult, EndResult, EnhanceResult, ExportResult, FailResult, HistoryResult, HowResult,
    ImportLearningsResult, InitResult, InvalidateLearningResult, IrrelevantResult, LearnResult,
    LearningsListResult, ListResult, MigrateResult, NextResult, RecallCmdResult, ResetResult,
    ReviewResult, RunDetailResult, SetupAuditResult, ShowResult, SkipResult, StatsResult,
    StatusResult, UnblockResult, UnskipResult, UpdateResult, WorktreesResult,
    format_apply_learning_text, format_begin_text, format_cheatsheet_text, format_complete_text,
    format_current_text, format_decisions_list_text, format_decline_text, format_doctor_text,
    format_end_text, format_enhance_text, format_export_text, format_fail_text,
    format_history_detail_text, format_history_text, format_how_text, format_import_learnings_text,
    format_init_text, format_invalidate_learning_text, format_irrelevant_text, format_learn_text,
    format_learnings_text, format_list_text, format_migrate_text, format_next_text,
    format_recall_text, format_reset_text, format_resolve_text, format_revert_text,
    format_review_text, format_setup_text, format_show_text, format_skip_text, format_stats_text,
    format_status_text, format_unblock_text, format_unskip_text, format_update_text,
    format_worktrees_text,
};
use crate::learnings::{
    DeleteLearningResult, EditLearningResult, format_delete_text, format_edit_text,
};
use crate::models::RunStatus;
use crate::output::ui;

// ============================================================================
// TextFormattable trait + generic output
// ============================================================================

/// Trait for result types that can be formatted as human-readable text.
///
/// Implementing this trait allows a result type to be used with the generic
/// [`output_result`] function, which handles both JSON and text output formats.
pub trait TextFormattable {
    /// Format this result as human-readable text for CLI output.
    fn format_text(&self) -> String;
}

/// Macro to implement TextFormattable by delegating to an existing standalone format function.
macro_rules! impl_text_formattable {
    ($type:ty, $format_fn:path) => {
        impl TextFormattable for $type {
            fn format_text(&self) -> String {
                $format_fn(self)
            }
        }
    };
}

impl_text_formattable!(crate::commands::AddResult, crate::commands::format_add_text);
impl_text_formattable!(CheatsheetResult, format_cheatsheet_text);
impl_text_formattable!(CurrentResult, format_current_text);
impl_text_formattable!(HowResult, format_how_text);
impl_text_formattable!(InitResult, format_init_text);
impl_text_formattable!(ListResult, format_list_text);
impl_text_formattable!(ShowResult, format_show_text);
impl_text_formattable!(NextResult, format_next_text);
impl_text_formattable!(CompleteResult, format_complete_text);
impl_text_formattable!(FailResult, format_fail_text);
impl_text_formattable!(BeginResult, format_begin_text);
impl_text_formattable!(UpdateResult, format_update_text);
impl_text_formattable!(EndResult, format_end_text);
impl_text_formattable!(ExportResult, format_export_text);
impl_text_formattable!(DoctorResult, format_doctor_text);
impl_text_formattable!(EnhanceResult, format_enhance_text);
impl_text_formattable!(SetupAuditResult, format_setup_text);
impl_text_formattable!(SkipResult, format_skip_text);
impl_text_formattable!(IrrelevantResult, format_irrelevant_text);
impl_text_formattable!(LearnResult, format_learn_text);
impl_text_formattable!(RecallCmdResult, format_recall_text);
impl_text_formattable!(LearningsListResult, format_learnings_text);
impl_text_formattable!(ImportLearningsResult, format_import_learnings_text);
impl_text_formattable!(UnblockResult, format_unblock_text);
impl_text_formattable!(UnskipResult, format_unskip_text);
impl_text_formattable!(ResetResult, format_reset_text);
impl_text_formattable!(StatsResult, format_stats_text);
impl_text_formattable!(HistoryResult, format_history_text);
impl_text_formattable!(RunDetailResult, format_history_detail_text);
impl_text_formattable!(DeleteLearningResult, format_delete_text);
impl_text_formattable!(EditLearningResult, format_edit_text);
impl_text_formattable!(ReviewResult, format_review_text);
impl_text_formattable!(StatusResult, format_status_text);
impl_text_formattable!(WorktreesResult, format_worktrees_text);
impl_text_formattable!(CountResult, format_count_text);
impl_text_formattable!(RetireResult, format_retire_text);
impl_text_formattable!(UnretireResult, format_unretire_text);
impl_text_formattable!(EnrichResult, format_enrich_text);
impl_text_formattable!(DedupResult, format_dedup_text);
impl_text_formattable!(EmbedResult, format_embed_text);
impl_text_formattable!(InvalidateLearningResult, format_invalidate_learning_text);
impl_text_formattable!(DecisionsListResult, format_decisions_list_text);
impl_text_formattable!(DecisionResolveResult, format_resolve_text);
impl_text_formattable!(DecisionDeclineResult, format_decline_text);
impl_text_formattable!(DecisionRevertResult, format_revert_text);
impl_text_formattable!(
    crate::loop_engine::archive::ArchiveResult,
    crate::loop_engine::archive::format_text
);
impl_text_formattable!(
    crate::loop_engine::status::DashboardResult,
    crate::loop_engine::status::format_text
);

// ApplyLearningResult: the original handler printed with a trailing newline.
// The standalone format function does NOT include a trailing newline,
// so we add one here to preserve identical output behavior.
impl TextFormattable for ApplyLearningResult {
    fn format_text(&self) -> String {
        format!("{}\n", format_apply_learning_text(self))
    }
}

/// Generic output function for any result type that implements both
/// `Serialize` (for JSON) and `TextFormattable` (for text).
///
/// Replaces the 30+ individual `output_xxx_result` functions with a single
/// generic function. Adding a new output format only requires modifying this
/// one function.
pub fn output_result<T: serde::Serialize + TextFormattable>(result: &T, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            output_json(result);
        }
        OutputFormat::Text => {
            write_stdout(&result.format_text());
        }
    }
}

/// Output MigrateResult based on format.
///
/// Kept as a standalone function because `format_migrate_text` requires
/// an extra `action` parameter that doesn't fit the `TextFormattable` trait.
pub fn output_migrate_result(result: &MigrateResult, format: OutputFormat, action: &str) {
    match format {
        OutputFormat::Json => {
            output_json(result);
        }
        OutputFormat::Text => {
            write_stdout(&format_migrate_text(result, action));
        }
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Convert CLI Shell type to clap_complete Shell type
pub fn convert_shell(shell: Shell) -> CompletionShell {
    match shell {
        Shell::Bash => CompletionShell::Bash,
        Shell::Zsh => CompletionShell::Zsh,
        Shell::Fish => CompletionShell::Fish,
        Shell::Powershell => CompletionShell::PowerShell,
    }
}

/// Convert CLI RunEndStatus to model RunStatus
pub fn convert_run_end_status(status: crate::cli::RunEndStatus) -> RunStatus {
    match status {
        crate::cli::RunEndStatus::Completed => RunStatus::Completed,
        crate::cli::RunEndStatus::Aborted => RunStatus::Aborted,
    }
}

/// Output JSON to stdout, or error message to stderr and exit with code 1.
///
/// This function exits the process on serialization failure rather than
/// silently outputting an empty object, which would mask bugs and
/// cause issues in automated pipelines.
pub fn output_json<T: serde::Serialize>(result: &T) {
    match serde_json::to_string_pretty(result) {
        Ok(json) => write_stdout(&format!("{}\n", json)),
        Err(e) => {
            ui::emit_err(&format!("Error: failed to serialize result to JSON: {e}"));
            process::exit(1);
        }
    }
}

// ============================================================================
// Panic-safe stdout writes (Unix OFD aliasing defense)
// ============================================================================
//
// The helpers below defend against a specific Unix-only hazard: when the
// caller does `task-mgr ... 2>&1 | tee ...`, fd 1 and fd 2 share an open file
// description (OFD). A libuv-backed child (the `claude` CLI) inherits fd 2 and
// can set O_NONBLOCK on the shared OFD. A subsequent large write via the
// result formatters then returns EAGAIN and panics inside std::print!.
//
// We therefore centralize *all* large result text/JSON emission through
// write_stdout (see output_result, output_migrate_result, output_json), which
// on Unix clears the bit before the write. The bug and its fix are entirely
// self-documented here; no CLAUDE.md footnote is needed.

#[cfg(unix)]
/// Clear `O_NONBLOCK` on `fd` if it is set.
///
/// See module docs above for the full OFD aliasing scenario.
fn clear_nonblocking(fd: libc::c_int) {
    // SAFETY: F_GETFL / F_SETFL on a valid fd take no pointers and only read or
    // write the fd's status flags; we clear a single bit and ignore the result.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 && (flags & libc::O_NONBLOCK) != 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
}

#[cfg(unix)]
/// Write all of `bytes` to `w` (backed by `fd`) without panicking.
///
/// Clears `O_NONBLOCK` on `fd` first (see [`clear_nonblocking`]); swallows
/// `BrokenPipe` (reader went away — nothing useful to do); exits with code 1
/// on any other write error.
///
/// We deliberately do NOT catch `WouldBlock` and retry: `write_all` may have
/// already flushed an unknown prefix, so re-writing `bytes` would duplicate.
/// The proactive flag clear avoids the need.
fn write_all_blocking<W: io::Write>(fd: libc::c_int, w: &mut W, bytes: &[u8]) {
    clear_nonblocking(fd);
    match w.write_all(bytes).and_then(|_| w.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            ui::emit_err(&format!("Error: failed to write to stdout: {e}"));
            process::exit(1);
        }
    }
}

#[cfg(unix)]
/// Write `s` to stdout without panicking on a transient `EAGAIN`.
fn write_stdout(s: &str) {
    use std::os::fd::AsRawFd;

    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let fd = lock.as_raw_fd();
    write_all_blocking(fd, &mut lock, s.as_bytes());
}

#[cfg(not(unix))]
/// Write `s` to stdout without panicking.
///
/// On non-Unix there is no O_NONBLOCK-via-OFD aliasing hazard from libuv
/// children (Windows pipe/handle semantics differ), so we emit directly while
/// preserving the exact BrokenPipe-silent / other-error-exit(1) contract.
fn write_stdout(s: &str) {
    use std::io::Write;

    let mut lock = io::stdout().lock();
    match lock.write_all(s.as_bytes()).and_then(|_| lock.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            ui::emit_err(&format!("Error: failed to write to stdout: {e}"));
            process::exit(1);
        }
    }
}

// ============================================================================
// Man page generation
// ============================================================================

/// Collect all man page names (main command + subcommands + nested subcommands)
fn collect_man_page_names() -> Vec<String> {
    let cmd = Cli::command();
    let mut names = vec!["task-mgr".to_string()];

    for subcmd in cmd.get_subcommands() {
        if subcmd.is_hide_set() {
            continue;
        }
        let name = subcmd.get_name();
        names.push(format!("task-mgr-{}", name));

        // Handle nested subcommands (e.g., run begin, run end, migrate status)
        for nested in subcmd.get_subcommands() {
            if nested.is_hide_set() {
                continue;
            }
            names.push(format!("task-mgr-{}-{}", name, nested.get_name()));
        }
    }

    names
}

/// Generate a single man page by name and return the rendered bytes
fn generate_man_page(name: &str) -> Result<Vec<u8>, TaskMgrError> {
    let cmd = Cli::command();

    // Main command
    if name == "task-mgr" {
        let man = Man::new(cmd);
        let mut buffer = Vec::new();
        man.render(&mut buffer)
            .map_err(|e| TaskMgrError::NotFound {
                resource_type: "man page render".to_string(),
                id: format!("{}: {}", name, e),
            })?;
        return Ok(buffer);
    }

    // Strip prefix to get subcommand path
    let subcmd_path = name
        .strip_prefix("task-mgr-")
        .ok_or_else(|| TaskMgrError::NotFound {
            resource_type: "man page".to_string(),
            id: name.to_string(),
        })?;

    // Find matching subcommand - subcommand names can contain hyphens (e.g., "delete-learning")
    // so we need to try different split points
    let target_cmd =
        find_subcommand_by_path(&cmd, subcmd_path).ok_or_else(|| TaskMgrError::NotFound {
            resource_type: "subcommand".to_string(),
            id: subcmd_path.to_string(),
        })?;

    // Render without renaming - the man page content will still be correct
    // as it uses the subcommand's documentation. The name in the man page
    // header comes from the command itself, which is fine.
    let man = Man::new(target_cmd);
    let mut buffer = Vec::new();
    man.render(&mut buffer)
        .map_err(|e| TaskMgrError::NotFound {
            resource_type: "man page render".to_string(),
            id: format!("{}: {}", name, e),
        })?;

    Ok(buffer)
}

/// Find a subcommand by a hyphen-separated path like "run-begin" or "delete-learning"
/// Returns the matching Command if found
fn find_subcommand_by_path(root: &clap::Command, path: &str) -> Option<clap::Command> {
    // First, try direct match (for subcommands with hyphens in their name like "delete-learning")
    if let Some(cmd) = root.get_subcommands().find(|c| c.get_name() == path) {
        return Some(cmd.clone());
    }

    // Try splitting at each hyphen position to find parent-child relationships
    // E.g., "run-begin" -> try "run" + "begin", "run-begin" (already tried above)
    // E.g., "migrate-status" -> try "migrate" + "status"
    for (i, _) in path.char_indices().filter(|(_, c)| *c == '-') {
        let (parent_name, rest) = path.split_at(i);
        let child_name = &rest[1..]; // Skip the hyphen

        if let Some(parent) = root.get_subcommands().find(|c| c.get_name() == parent_name) {
            // Check if there's a matching child subcommand
            if let Some(child) = parent
                .get_subcommands()
                .find(|c| c.get_name() == child_name)
            {
                return Some(child.clone());
            }
            // Also try recursive matching for deeper nesting (not needed currently but future-proof)
            if let Some(found) = find_subcommand_by_path(parent, child_name) {
                return Some(found);
            }
        }
    }

    None
}

/// Generate man pages based on options
pub fn generate_man_pages(
    output_dir: Option<&Path>,
    name: Option<&str>,
    list: bool,
) -> Result<(), TaskMgrError> {
    let all_names = collect_man_page_names();

    // List mode: just print names
    if list {
        ui::emit_data("Available man pages:");
        for n in &all_names {
            ui::emit_data(&format!("  {n}.1"));
        }
        return Ok(());
    }

    // Single name mode: output to stdout
    if let Some(requested_name) = name {
        let buffer = generate_man_page(requested_name)?;
        io::Write::write_all(&mut io::stdout(), &buffer)?;
        return Ok(());
    }

    // Output directory mode: generate all man pages to files
    if let Some(dir) = output_dir {
        // Create directory if it doesn't exist
        fs::create_dir_all(dir)?;

        let mut generated = 0;
        for n in &all_names {
            let buffer = generate_man_page(n)?;
            let file_path = dir.join(format!("{}.1", n));
            fs::write(&file_path, &buffer)?;
            generated += 1;
        }

        ui::emit_data(&format!(
            "Generated {} man pages in {}",
            generated,
            dir.display()
        ));
        return Ok(());
    }

    // No options provided - show help
    ui::emit("Usage: task-mgr man-pages --output-dir <DIR> | --name <NAME> | --list");
    ui::emit("       Use --output-dir to generate all man pages to a directory");
    ui::emit("       Use --name to generate a single man page to stdout");
    ui::emit("       Use --list to see available man page names");
    Ok(())
}

// ============================================================================
// Shell completions generation
// ============================================================================

/// Generate shell completions and output to stdout
pub fn generate_completions(shell: Shell) {
    let completion_shell = convert_shell(shell);
    let mut cmd = Cli::command();
    generate(completion_shell, &mut cmd, "task-mgr", &mut io::stdout());
}

#[cfg(all(test, unix))]
mod stdout_write_tests {
    use super::{clear_nonblocking, write_all_blocking};
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::thread;

    /// Create a pipe, returning (read_fd, write_fd). Linux default buffer is 64 KB.
    fn make_pipe() -> (libc::c_int, libc::c_int) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe() writes two valid fds into the 2-element array we provide.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        (fds[0], fds[1])
    }

    fn set_nonblocking(fd: libc::c_int) {
        // SAFETY: F_GETFL/F_SETFL on a valid fd; we set a single status-flag bit.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            assert!(flags >= 0, "F_GETFL failed");
            assert_eq!(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK), 0);
        }
    }

    fn nonblock_bit(fd: libc::c_int) -> libc::c_int {
        // SAFETY: F_GETFL on a valid fd reads its status flags, takes no pointers.
        unsafe { libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK }
    }

    const PAYLOAD_LEN: usize = 128 * 1024; // > 64 KB pipe buffer

    #[test]
    fn clear_nonblocking_clears_the_flag() {
        let (read_fd, write_fd) = make_pipe();
        set_nonblocking(write_fd);
        assert_ne!(nonblock_bit(write_fd), 0, "precondition: O_NONBLOCK set");

        clear_nonblocking(write_fd);

        assert_eq!(nonblock_bit(write_fd), 0, "O_NONBLOCK should be cleared");
        // SAFETY: both fds are open and owned by this test; close once each.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    #[test]
    fn write_all_blocking_delivers_full_payload_over_nonblocking_pipe() {
        let (read_fd, write_fd) = make_pipe();
        set_nonblocking(write_fd);

        // Reader thread owns the read end (closes it on drop) and drains until EOF.
        let reader = thread::spawn(move || {
            // SAFETY: read_fd is a fresh pipe fd moved into this thread; File owns it.
            let mut rf = unsafe { File::from_raw_fd(read_fd) };
            let mut buf = Vec::new();
            rf.read_to_end(&mut buf).expect("reader failed");
            buf.len()
        });

        // SAFETY: write_fd is a fresh pipe fd; File owns it and closes on drop.
        let mut wf = unsafe { File::from_raw_fd(write_fd) };
        let payload = vec![0xABu8; PAYLOAD_LEN];
        write_all_blocking(write_fd, &mut wf, &payload);
        drop(wf); // EOF for the reader

        let total = reader.join().expect("reader thread panicked");
        assert_eq!(total, PAYLOAD_LEN, "all bytes must be delivered");
    }

    #[test]
    fn raw_write_all_wouldblock_recreates_the_bug() {
        // Negative control: prove the regression mechanism. With O_NONBLOCK set and
        // no reader draining, a raw write_all of a >64 KB payload must WouldBlock.
        let (read_fd, write_fd) = make_pipe();
        set_nonblocking(write_fd);

        // SAFETY: write_fd is a fresh pipe fd; File owns it and closes on drop.
        let mut wf = unsafe { File::from_raw_fd(write_fd) };
        let payload = vec![0u8; PAYLOAD_LEN];
        let err = wf
            .write_all(&payload)
            .expect_err("write_all should block-fail on a full non-blocking pipe");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);

        // SAFETY: read_fd is open and owned here; wf closes write_fd on drop.
        unsafe { libc::close(read_fd) };
    }

    #[test]
    fn write_all_blocking_swallows_broken_pipe() {
        let (read_fd, write_fd) = make_pipe();
        // Close the read end first so the write hits EPIPE -> BrokenPipe.
        // (Rust's runtime ignores SIGPIPE, so this surfaces as an error, not a signal.)
        // SAFETY: read_fd is open and owned by this test.
        unsafe { libc::close(read_fd) };

        // SAFETY: write_fd is a fresh pipe fd; File owns it and closes on drop.
        let mut wf = unsafe { File::from_raw_fd(write_fd) };
        // Returns quietly (no panic, no process::exit) — the test surviving proves it.
        write_all_blocking(write_fd, &mut wf, &vec![0u8; 1024]);
    }

    #[test]
    fn write_all_blocking_attempts_write_when_fgetfl_fails() {
        // fd -1 makes clear_nonblocking's F_GETFL return -1; it must not panic and
        // the write must still proceed against the provided writer.
        let mut sink: Vec<u8> = Vec::new();
        write_all_blocking(-1, &mut sink, b"hello");
        assert_eq!(sink, b"hello");
    }
}
