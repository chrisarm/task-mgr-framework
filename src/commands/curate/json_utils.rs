//! Shared JSON extraction utilities for curate subcommands.

/// Finds a JSON array in the response text, handling markdown code blocks.
pub(super) fn extract_json_array(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // Try raw JSON array first
    if trimmed.starts_with('[') {
        if let Some(end) = find_matching_bracket(trimmed) {
            return Some(trimmed[..=end].to_string());
        }
    }

    // Try markdown code block: ```json\n...\n```
    if let Some(start) = trimmed.find("```json") {
        let after_marker = start + "```json".len();
        if let Some(end) = trimmed[after_marker..].find("```") {
            let json = trimmed[after_marker..after_marker + end].trim();
            return Some(json.to_string());
        }
    }

    // Try generic code block: ```\n...\n```
    if let Some(start) = trimmed.find("```\n") {
        let after_marker = start + "```\n".len();
        if let Some(end) = trimmed[after_marker..].find("```") {
            let json = trimmed[after_marker..after_marker + end].trim();
            if json.starts_with('[') {
                return Some(json.to_string());
            }
        }
    }

    None
}

/// Finds the index of the closing bracket matching the opening bracket at index 0.
pub(super) fn find_matching_bracket(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in text.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}
