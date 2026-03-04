//! Accumulated session guidance from `.pause` interactions.
//!
//! `SessionGuidance` builds up user-provided context across multiple
//! pause/resume cycles. It is conceptually separate from signal file
//! handling (stop/pause file checks) which lives in `signals.rs`.

use chrono;

/// Accumulated session guidance from `.pause` interactions.
///
/// Each pause interaction appends guidance with an iteration tag,
/// building up context across multiple pause/resume cycles.
#[derive(Debug, Default)]
pub struct SessionGuidance {
    entries: Vec<GuidanceEntry>,
}

/// A single guidance entry from one pause interaction.
#[derive(Debug)]
struct GuidanceEntry {
    iteration: u32,
    text: String,
}

impl SessionGuidance {
    /// Create a new empty SessionGuidance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add guidance from a pause interaction at the given iteration.
    pub fn add(&mut self, iteration: u32, text: String) {
        if !text.trim().is_empty() {
            self.entries.push(GuidanceEntry { iteration, text });
        }
    }

    /// Format all accumulated guidance for inclusion in the prompt.
    ///
    /// Returns empty string if no guidance has been recorded.
    pub fn format_for_prompt(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut result = String::new();
        for entry in &self.entries {
            result.push_str(&format!(
                "[Iteration {}] {}\n",
                entry.iteration,
                entry.text.trim()
            ));
        }
        result
    }

    /// Whether any guidance has been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Format all accumulated guidance for recording to progress.txt.
    ///
    /// Produces a structured progress entry with a "Session Guidance" header
    /// and all entries listed with their iteration numbers.
    /// Returns empty string if no guidance has been recorded.
    pub fn format_for_recording(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let mut result = format!("\n## {} - Session Guidance\n", timestamp);

        for entry in &self.entries {
            result.push_str(&format!(
                "- [Iteration {}] {}\n",
                entry.iteration,
                entry.text.trim()
            ));
        }
        result.push_str("---\n");
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_guidance_new_is_empty() {
        let guidance = SessionGuidance::new();
        assert!(guidance.is_empty());
        assert!(guidance.format_for_prompt().is_empty());
    }

    #[test]
    fn test_session_guidance_add_and_format() {
        let mut guidance = SessionGuidance::new();
        guidance.add(3, "Focus on error handling".to_string());

        assert!(!guidance.is_empty());
        let formatted = guidance.format_for_prompt();
        assert!(formatted.contains("[Iteration 3]"));
        assert!(formatted.contains("Focus on error handling"));
    }

    #[test]
    fn test_session_guidance_accumulates_multiple_entries() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "First guidance".to_string());
        guidance.add(5, "Second guidance".to_string());
        guidance.add(10, "Third guidance".to_string());

        let formatted = guidance.format_for_prompt();
        assert!(formatted.contains("[Iteration 1]"));
        assert!(formatted.contains("First guidance"));
        assert!(formatted.contains("[Iteration 5]"));
        assert!(formatted.contains("Second guidance"));
        assert!(formatted.contains("[Iteration 10]"));
        assert!(formatted.contains("Third guidance"));
    }

    #[test]
    fn test_session_guidance_ignores_empty_text() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "".to_string());
        guidance.add(2, "   \n  ".to_string());

        assert!(guidance.is_empty());
        assert!(guidance.format_for_prompt().is_empty());
    }

    #[test]
    fn test_session_guidance_trims_text() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "  padded text  ".to_string());

        let formatted = guidance.format_for_prompt();
        assert!(formatted.contains("padded text"));
    }

    // --- SessionGuidance format_for_recording tests ---

    #[test]
    fn test_format_for_recording_empty_returns_empty() {
        let guidance = SessionGuidance::new();
        assert!(guidance.format_for_recording().is_empty());
    }

    #[test]
    fn test_format_for_recording_single_entry() {
        let mut guidance = SessionGuidance::new();
        guidance.add(3, "Focus on error handling".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.contains("Session Guidance"));
        assert!(formatted.contains("[Iteration 3] Focus on error handling"));
        assert!(formatted.contains("---"));
    }

    #[test]
    fn test_format_for_recording_multiple_entries() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "First guidance".to_string());
        guidance.add(5, "Second guidance".to_string());
        guidance.add(10, "Third guidance".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.contains("Session Guidance"));
        assert!(formatted.contains("[Iteration 1] First guidance"));
        assert!(formatted.contains("[Iteration 5] Second guidance"));
        assert!(formatted.contains("[Iteration 10] Third guidance"));
        assert!(formatted.contains("---"));
    }

    #[test]
    fn test_format_for_recording_has_timestamp() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        let formatted = guidance.format_for_recording();
        // Should contain a UTC timestamp in the header
        assert!(formatted.contains("UTC"));
        // Should have the ## header format for progress.txt
        assert!(formatted.contains("## "));
    }

    #[test]
    fn test_format_for_recording_trims_entry_text() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "  padded text  ".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.contains("[Iteration 1] padded text"));
        // Should not contain leading/trailing spaces in the entry
        assert!(!formatted.contains("[Iteration 1]   padded text"));
    }

    #[test]
    fn test_format_for_recording_starts_with_newline() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        let formatted = guidance.format_for_recording();
        // Should start with newline for clean appending to progress.txt
        assert!(formatted.starts_with('\n'));
    }

    #[test]
    fn test_format_for_recording_ends_with_separator() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        let formatted = guidance.format_for_recording();
        assert!(formatted.ends_with("---\n"));
    }
}
