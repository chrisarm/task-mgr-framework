//! `ui::` — the product output channel (CONTRACT-LOG-001 channels A / A2).
//!
//! These helpers carry **human-facing progress**, **byte-locked operator
//! contracts**, and **machine-readable CLI data** to the terminal. They are
//! deliberately *dumb*: each writes the caller's exact bytes plus a single
//! trailing newline (or a per-line prefix) and **never** prepends a log level,
//! timestamp, target, or ANSI decoration. That byte-for-byte fidelity is what
//! lets operator-grepped contract lines (e.g. `lifecycle_stderr_contract.rs`)
//! and snapshot tests keep passing after the logging migration.
//!
//! This is the *opposite* discipline from [`crate::observability`] (channel B),
//! where `tracing` events are decorated with level + timestamp and filtered by
//! `TASK_MGR_LOG`. Diagnostics go to `tracing`; product output goes here.
//!
//! FD discipline:
//! - [`emit`], [`emit_err`], [`emit_prefixed`] → **stderr** (FD 2)
//! - [`emit_data`] → **stdout** (FD 1)
//!
//! Writes go straight to the locked `stderr`/`stdout` handles rather than the
//! `print!`/`eprint!` macro family, so they reach the real file descriptor even
//! under libtest's thread-local output capture — the `dup2`-based snapshot
//! tests depend on this (learning #3295).

use std::io::Write;

/// Emit a line of human-facing progress to **stderr** (FD 2).
///
/// Writes `msg` followed by a single `\n`; no level/timestamp/color is added.
pub fn emit(msg: &str) {
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "{msg}");
}

/// Emit an error or warning line to **stderr** (FD 2).
///
/// Same wire format as [`emit`] (the caller's bytes, undecorated); the separate
/// name documents intent at the call site and keeps error routing greppable.
pub fn emit_err(msg: &str) {
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "{msg}");
}

/// Emit machine-readable / CLI data to **stdout** (FD 1).
///
/// `list` / `show` / `stats` / `export` output that scripts and pipes consume
/// belongs here so `task-mgr show … | grep …` keeps working. No decoration.
pub fn emit_data(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
}

/// Emit `text` to **stderr** (FD 2) with `slot_label` prepended to every line.
///
/// Re-home of `loop_engine::claude::emit_prefixed_lines` (the per-slot tee).
/// The byte semantics are locked by [`write_prefixed`]'s tests and preserved
/// exactly:
/// - `None` label → a single `writeln!(text)` (legacy unprefixed path).
/// - `Some(label)` → each `\n`-split line becomes `"{label} {line}"`.
/// - empty `text` with a label → one prefixed blank line, so a slot's "I said
///   nothing this turn" still shows up and stays attributable.
/// - interior blank lines are preserved as prefixed blanks.
///
/// (The original `claude::emit_prefixed_lines` stays in place until its callers
/// migrate in FEAT-002/003; this is the new single home for the semantics.)
pub fn emit_prefixed(slot_label: Option<&str>, text: &str) {
    let mut out = std::io::stderr().lock();
    let _ = write_prefixed(&mut out, slot_label, text);
}

/// Inner, testable core of [`emit_prefixed`]: writes to an arbitrary `Write` so
/// the line-splitting/prefixing logic can be asserted byte-for-byte.
///
/// Returns `io::Result<()>` so a failed write surfaces in tests; production
/// callers via [`emit_prefixed`] ignore it (a stderr write failure is not
/// actionable).
pub(crate) fn write_prefixed<W: Write>(
    out: &mut W,
    slot_label: Option<&str>,
    text: &str,
) -> std::io::Result<()> {
    match slot_label {
        None => writeln!(out, "{text}"),
        Some(prefix) => {
            // An empty `text` produces zero `lines()` — emit one prefixed blank
            // so the slot's silent turn still shows up.
            if text.is_empty() {
                return writeln!(out, "{prefix}");
            }
            // `str::lines()` splits on `\n`/`\r\n` and drops a trailing empty
            // token, matching the trailing-newline semantics of a single
            // `writeln!("{}", text)`. Interior blank lines stay as empty strings
            // so they still get a prefix (and remain attributable).
            for line in text.lines() {
                writeln!(out, "{prefix} {line}")?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive [`write_prefixed`] against an in-memory buffer so we can assert the
    /// exact bytes — the negative-decoration contract (input + newline/prefix
    /// only, no level/timestamp).
    fn write_to_string(slot_label: Option<&str>, text: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_prefixed(&mut buf, slot_label, text).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn none_prefix_is_plain_passthrough() {
        // No prefix: identical to a single writeln!("{}", text); no decoration.
        assert_eq!(write_to_string(None, "hello"), "hello\n");
        assert_eq!(write_to_string(None, "a\nb"), "a\nb\n");
        assert_eq!(write_to_string(None, ""), "\n");
    }

    #[test]
    fn single_line_gets_one_prefix() {
        assert_eq!(
            write_to_string(Some("[slot 0]"), "hello"),
            "[slot 0] hello\n"
        );
    }

    #[test]
    fn multi_line_each_line_prefixed() {
        assert_eq!(
            write_to_string(Some("[slot 1]"), "line one\nline two\nline three"),
            "[slot 1] line one\n[slot 1] line two\n[slot 1] line three\n"
        );
    }

    #[test]
    fn trailing_newline_dropped() {
        // Matches eprintln!("{}", "a\nb\n"): writeln adds one newline and
        // str::lines() drops the trailing empty token — no stray prefixed blank.
        assert_eq!(
            write_to_string(Some("[slot 0]"), "a\nb\n"),
            "[slot 0] a\n[slot 0] b\n"
        );
    }

    #[test]
    fn empty_text_with_prefix_is_one_blank_prefixed_line() {
        assert_eq!(write_to_string(Some("[slot 2]"), ""), "[slot 2]\n");
    }

    #[test]
    fn interior_blank_line_preserved() {
        // A deliberately blank line between two non-empty lines is preserved as
        // a prefixed-blank line so the slot's blank line stays attributable.
        assert_eq!(
            write_to_string(Some("[slot 2]"), "a\n\nb"),
            "[slot 2] a\n[slot 2] \n[slot 2] b\n"
        );
    }
}
