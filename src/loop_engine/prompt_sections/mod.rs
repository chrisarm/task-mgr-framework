//! Prompt section builders for the autonomous agent loop.
//!
//! Each sub-module assembles one named section of the prompt string.
//! Shared utilities (e.g. `truncate_to_budget`) live in this module
//! and are available to all section builders via `pub(crate)`.

pub mod dependencies;
pub mod escalation;
pub mod learnings;
pub mod siblings;
pub mod synergy;
pub mod task_ops;

/// Truncate a string to fit within a byte budget.
///
/// If the text exceeds the budget, it is sliced at the nearest valid UTF-8
/// character boundary and a truncation notice is appended.
pub(crate) fn truncate_to_budget(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        text.to_string()
    } else {
        let safe_end = text.floor_char_boundary(budget);
        let truncated = &text[..safe_end];
        format!("{}...\n[truncated to {} bytes]", truncated, budget)
    }
}

/// Try to fit a prompt section into the remaining byte budget.
///
/// Returns the section verbatim if it fits, or an empty string when it
/// doesn't (with a stderr warning). Empty sections pass through unchanged
/// without consuming budget. When a non-empty section is dropped, its
/// `name` is appended to `dropped` so callers can report the drop to
/// observability surfaces (overflow dumps, JSONL events).
///
/// Shared between the sequential and slot prompt builders so both paths
/// account for budget consumption identically. See `prompt::sequential` and
/// `prompt::slot` for usage.
pub(crate) fn try_fit_section(
    section: String,
    name: &str,
    remaining: &mut usize,
    dropped: &mut Vec<String>,
) -> String {
    if section.is_empty() {
        return section;
    }
    if section.len() <= *remaining {
        *remaining -= section.len();
        section
    } else {
        eprintln!(
            "Warning: {} section ({} bytes) skipped — only {} bytes remaining in prompt budget",
            name,
            section.len(),
            remaining,
        );
        dropped.push(name.to_string());
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_to_budget_within_limit() {
        let text = "short text";
        let result = truncate_to_budget(text, 100);
        assert_eq!(result, "short text");
    }

    #[test]
    fn test_truncate_to_budget_exceeds_limit() {
        let text = "a".repeat(5000);
        let result = truncate_to_budget(&text, 100);
        assert!(result.len() < 200);
        assert!(result.contains("[truncated to 100 bytes]"));
    }

    #[test]
    fn test_truncate_to_budget_exact_limit() {
        let text = "abcde";
        let result = truncate_to_budget(text, 5);
        assert_eq!(result, "abcde");
    }

    #[test]
    fn test_truncate_to_budget_zero() {
        let result = truncate_to_budget("hello", 0);
        assert!(
            result.contains("[truncated to 0 bytes]"),
            "Zero budget should truncate"
        );
    }

    #[test]
    fn test_truncate_to_budget_one_char() {
        let result = truncate_to_budget("hello", 1);
        assert!(result.starts_with('h'));
        assert!(result.contains("[truncated to 1 bytes]"));
    }

    #[test]
    fn test_truncate_to_budget_empty_string() {
        let result = truncate_to_budget("", 100);
        assert_eq!(result, "", "Empty string within budget returns empty");
    }

    #[test]
    fn test_truncate_to_budget_multibyte_utf8_no_panic() {
        // "café" = 5 chars, 6 bytes (é is 2 bytes: 0xC3 0xA9)
        let text = "café";
        assert_eq!(text.len(), 5); // 5 bytes
        // Budget 4 falls after 'f' but before 'é' starts — safe
        let result = truncate_to_budget(text, 4);
        assert!(result.contains("[truncated to 4 bytes]"));
        assert!(result.starts_with("caf"));
        // Budget 3 falls mid-way — would panic with naive slicing if é started at byte 3
        let result = truncate_to_budget(text, 3);
        assert!(result.contains("[truncated to 3 bytes]"));
    }

    #[test]
    fn test_truncate_to_budget_emoji_no_panic() {
        // Each emoji is 4 bytes
        let text = "🍕🍔🌮🍣";
        assert_eq!(text.len(), 16); // 4 emoji × 4 bytes
        // Budget 5 falls mid-second emoji (byte 5 is inside 🍔)
        let result = truncate_to_budget(text, 5);
        assert!(result.contains("[truncated to 5 bytes]"));
        // Should contain only first emoji (4 bytes), not a partial second
        assert!(result.starts_with("🍕"));
        assert!(!result.starts_with("🍕🍔"));
    }

    #[test]
    fn test_truncate_to_budget_cjk_no_panic() {
        // CJK characters are 3 bytes each
        let text = "你好世界";
        assert_eq!(text.len(), 12); // 4 chars × 3 bytes
        // Budget 4 falls mid-second character (byte 4 is inside 好)
        let result = truncate_to_budget(text, 4);
        assert!(result.contains("[truncated to 4 bytes]"));
        assert!(result.starts_with("你"));
    }

    #[test]
    fn test_truncate_to_budget_mixed_ascii_and_multibyte() {
        let text = "hello 世界!";
        // h(1) e(1) l(1) l(1) o(1) (1) 世(3) 界(3) !(1) = 13 bytes
        assert_eq!(text.len(), 13);
        // Budget 7 = just past the space, before 世 starts (byte 6 is space, 7 is mid-世)
        let result = truncate_to_budget(text, 7);
        assert!(result.contains("[truncated to 7 bytes]"));
        assert!(result.starts_with("hello "));
    }
}
