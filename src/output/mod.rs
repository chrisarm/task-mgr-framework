//! Product/operator output channel.
//!
//! [`ui`] is the CONTRACT-LOG-001 channels A/A2 surface: human-facing progress,
//! byte-locked operator-contract lines, and machine-readable CLI data, each
//! written to its audience FD with **no** level/timestamp decoration. Internal
//! diagnostics (channel B) live in [`crate::observability`] (`tracing`) instead.
//!
//! The decorated helpers below ([`warn`]) own the color/NO_COLOR/TTY discipline
//! via [`should_color`]; the raw `ui::` passthrough emitters deliberately do not
//! decorate, so they preserve exact bytes for snapshot tests and operator greps.

pub mod ui;

/// Emit a `[warn]` line to stderr.
///
/// Color (yellow ANSI) is suppressed when `NO_COLOR` is set (any value, per
/// <https://no-color.org>) or when stderr is not a real TTY.
pub fn warn(msg: &str) {
    eprintln!("{}", format_warn(msg, should_color()));
}

/// Format a warn line; `color` controls ANSI escapes.
pub fn format_warn(msg: &str, color: bool) -> String {
    if color {
        format!("\x1b[33m[warn]\x1b[0m {msg}")
    } else {
        format!("[warn] {msg}")
    }
}

fn should_color() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_warn_no_color_has_no_ansi() {
        let s = format_warn("something bad", false);
        assert!(!s.contains('\x1b'));
        assert!(s.contains("[warn]"));
        assert!(s.contains("something bad"));
    }

    #[test]
    fn format_warn_color_has_ansi() {
        let s = format_warn("something bad", true);
        assert!(s.contains("\x1b[33m"));
        assert!(s.contains("[warn]"));
        assert!(s.contains("something bad"));
    }

    #[test]
    fn no_color_env_var_disables_color() {
        // In a test process stderr is not a TTY, so should_color() is already
        // false. Setting NO_COLOR is belt-and-suspenders: we verify that the
        // no-ANSI path activates regardless of terminal detection.
        //
        // SAFETY: single-threaded test; no concurrent env reads in this test.
        unsafe {
            std::env::set_var("NO_COLOR", "1");
        }
        let color = should_color();
        unsafe {
            std::env::remove_var("NO_COLOR");
        }
        assert!(!color, "NO_COLOR=1 must disable color");
    }
}
