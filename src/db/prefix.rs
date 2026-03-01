//! Shared prefix utility for SQL LIKE-based task ID filtering.
//!
//! Provides helpers for constructing safe SQLite LIKE clauses that scope
//! queries to a specific PRD prefix (e.g. `P1` → `WHERE id LIKE ? ESCAPE '\'`
//! with pattern `P1-%`).
//!
//! # Safety
//!
//! SQLite LIKE treats `%`, `_`, and the escape character as special. When the
//! prefix itself contains these characters the naive `format!("{prefix}-%")`
//! would produce an unintended wildcard. `escape_like` handles escaping and
//! `validate_prefix` provides a front-door guard that rejects dangerous input.
//!
//! # Implementation note (FEAT-001)
//!
//! Function bodies are `todo!()` stubs. They are replaced in FEAT-001.
//! Tests in this module are expected to **fail** until that task is complete.

// LIKE escape character used throughout this module.
const ESCAPE_CHAR: char = '\\';

/// Escape SQLite LIKE special characters in `s` using backslash as the escape
/// character.
///
/// | Input char | Output     |
/// |-----------|------------|
/// | `%`       | `\%`       |
/// | `_`       | `\_`       |
/// | `\`       | `\\`       |
///
/// All other characters are passed through unchanged.
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push(ESCAPE_CHAR);
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Build the LIKE pattern for a prefix: `"{escaped_prefix}-%"`.
///
/// Use this when you need the raw pattern without a SQL clause fragment,
/// e.g. when the same pattern is bound to multiple statements.
pub fn make_like_pattern(prefix: &str) -> String {
    format!("{}-%", escape_like(prefix))
}

/// Build a `WHERE {col} LIKE ? ESCAPE '\\'` clause and LIKE pattern.
///
/// Returns the clause string and `Some(pattern)` when a prefix is supplied,
/// or `("", None)` when `prefix` is `None`. The column name is embedded
/// directly into the SQL fragment — callers are responsible for passing a
/// trusted column name (not user input).
pub fn prefix_where_col(col: &str, prefix: Option<&str>) -> (String, Option<String>) {
    match prefix {
        Some(p) => {
            let pattern = make_like_pattern(p);
            (format!("WHERE {col} LIKE ? ESCAPE '\\'"), Some(pattern))
        }
        None => (String::new(), None),
    }
}

/// Build an `AND {col} LIKE ? ESCAPE '\\'` clause and LIKE pattern.
///
/// Identical to [`prefix_where_col`] but uses `AND`, suitable for appending
/// to a query that already has a `WHERE` clause.
pub fn prefix_and_col(col: &str, prefix: Option<&str>) -> (String, Option<String>) {
    match prefix {
        Some(p) => {
            let pattern = make_like_pattern(p);
            (format!("AND {col} LIKE ? ESCAPE '\\'"), Some(pattern))
        }
        None => (String::new(), None),
    }
}

/// Build a `WHERE` clause and LIKE pattern for filtering tasks by PRD prefix.
///
/// Convenience wrapper around [`prefix_where_col`] that uses the `id` column.
/// Returns `("WHERE id LIKE ? ESCAPE '\\'", Some("{prefix}-%"))` when a prefix
/// is supplied, or `("", None)` when `prefix` is `None`.
pub fn prefix_where(prefix: Option<&str>) -> (String, Option<String>) {
    prefix_where_col("id", prefix)
}

/// Build an `AND` clause and LIKE pattern for filtering tasks by PRD prefix.
///
/// Convenience wrapper around [`prefix_and_col`] that uses the `id` column.
/// Suitable for appending to a query that already has a `WHERE` clause.
pub fn prefix_and(prefix: Option<&str>) -> (String, Option<String>) {
    prefix_and_col("id", prefix)
}

/// Validate that a prefix string contains only safe characters.
///
/// Allowed characters: `[a-zA-Z0-9.-]` (alphanumeric, dot, dash).
///
/// Rejected characters include `%`, `_`, `/`, `\`, whitespace, and empty
/// string — all of which would either produce unintended SQL LIKE wildcards
/// or be ambiguous as separators.
///
/// # Errors
///
/// Returns `Err(String)` with a human-readable reason if the prefix is invalid.
pub fn validate_prefix(prefix: &str) -> Result<(), String> {
    if prefix.is_empty() {
        return Err("prefix must not be empty".to_string());
    }
    for ch in prefix.chars() {
        if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-') {
            return Err(format!(
                "prefix contains invalid character {:?}; only [a-zA-Z0-9.-] are allowed",
                ch
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // escape_like
    // -----------------------------------------------------------------------

    #[test]
    fn test_escape_like_percent() {
        assert_eq!(escape_like("100%"), "100\\%");
    }

    #[test]
    fn test_escape_like_underscore() {
        assert_eq!(escape_like("foo_bar"), "foo\\_bar");
    }

    #[test]
    fn test_escape_like_backslash() {
        assert_eq!(escape_like("foo\\bar"), "foo\\\\bar");
    }

    #[test]
    fn test_escape_like_all_special_chars() {
        // All three special chars in sequence: %_\  →  \%\_\\
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
    }

    #[test]
    fn test_escape_like_no_special_chars() {
        // Plain alphanumeric prefix is returned unchanged.
        assert_eq!(escape_like("P1"), "P1");
    }

    #[test]
    fn test_escape_like_empty_string() {
        assert_eq!(escape_like(""), "");
    }

    // -----------------------------------------------------------------------
    // prefix_where
    // -----------------------------------------------------------------------

    #[test]
    fn test_prefix_where_some() {
        let (clause, param) = prefix_where(Some("P1"));
        assert_eq!(clause, "WHERE id LIKE ? ESCAPE '\\'");
        assert_eq!(param, Some("P1-%".to_string()));
    }

    #[test]
    fn test_prefix_where_none() {
        let (clause, param) = prefix_where(None);
        assert_eq!(clause, "");
        assert_eq!(param, None);
    }

    #[test]
    fn test_prefix_where_includes_escape_clause() {
        // Ensure the ESCAPE clause is always present when prefix is Some.
        let (clause, _) = prefix_where(Some("ABC"));
        assert!(
            clause.contains("ESCAPE"),
            "WHERE clause must include ESCAPE to protect against LIKE wildcards in prefix"
        );
    }

    #[test]
    fn test_prefix_where_pattern_has_dash_separator() {
        // The dash separator is required so that prefix "P1" does not
        // accidentally match tasks belonging to a different prefix like "P10".
        let (_, param) = prefix_where(Some("P1"));
        let pattern = param.unwrap();
        assert!(
            pattern.starts_with("P1-"),
            "pattern must start with 'P1-' (dash separator), got: {pattern}"
        );
    }

    // -----------------------------------------------------------------------
    // prefix_and
    // -----------------------------------------------------------------------

    #[test]
    fn test_prefix_and_some() {
        let (clause, param) = prefix_and(Some("P1"));
        assert_eq!(clause, "AND id LIKE ? ESCAPE '\\'");
        assert_eq!(param, Some("P1-%".to_string()));
    }

    #[test]
    fn test_prefix_and_none() {
        let (clause, param) = prefix_and(None);
        assert_eq!(clause, "");
        assert_eq!(param, None);
    }

    #[test]
    fn test_prefix_and_includes_escape_clause() {
        let (clause, _) = prefix_and(Some("XYZ"));
        assert!(clause.contains("ESCAPE"));
    }

    // -----------------------------------------------------------------------
    // validate_prefix — accepted characters
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_prefix_accepts_lowercase_alpha() {
        assert!(validate_prefix("abc").is_ok());
    }

    #[test]
    fn test_validate_prefix_accepts_uppercase_alpha() {
        assert!(validate_prefix("ABC").is_ok());
    }

    #[test]
    fn test_validate_prefix_accepts_digits() {
        assert!(validate_prefix("123").is_ok());
    }

    #[test]
    fn test_validate_prefix_accepts_mixed_alphanumeric() {
        assert!(validate_prefix("P1").is_ok());
        assert!(validate_prefix("abc123").is_ok());
        assert!(validate_prefix("MyProject2").is_ok());
    }

    #[test]
    fn test_validate_prefix_accepts_dot() {
        assert!(validate_prefix("v1.2").is_ok());
        assert!(validate_prefix("project.name").is_ok());
    }

    #[test]
    fn test_validate_prefix_accepts_dash() {
        assert!(validate_prefix("my-project").is_ok());
        assert!(validate_prefix("feat-branch").is_ok());
    }

    // -----------------------------------------------------------------------
    // validate_prefix — rejected characters
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_prefix_rejects_empty_string() {
        assert!(
            validate_prefix("").is_err(),
            "empty prefix must be rejected"
        );
    }

    #[test]
    fn test_validate_prefix_rejects_percent() {
        assert!(
            validate_prefix("P1%").is_err(),
            "percent sign is a LIKE wildcard and must be rejected"
        );
    }

    #[test]
    fn test_validate_prefix_rejects_underscore() {
        // Underscore is a LIKE single-char wildcard. A prefix like "P_1" would
        // match "PA1", "PB1", … in an unescaped LIKE clause.
        assert!(
            validate_prefix("P_1").is_err(),
            "underscore is a LIKE wildcard and must be rejected"
        );
    }

    #[test]
    fn test_validate_prefix_rejects_forward_slash() {
        assert!(
            validate_prefix("P1/sub").is_err(),
            "forward slash is not a valid prefix character"
        );
    }

    #[test]
    fn test_validate_prefix_rejects_backslash() {
        assert!(
            validate_prefix("P1\\sub").is_err(),
            "backslash is the LIKE escape character and must be rejected in prefixes"
        );
    }

    #[test]
    fn test_validate_prefix_rejects_space() {
        assert!(
            validate_prefix("P 1").is_err(),
            "spaces are not allowed in task prefixes"
        );
    }

    #[test]
    fn test_validate_prefix_rejects_tab() {
        assert!(validate_prefix("P\t1").is_err());
    }

    #[test]
    fn test_validate_prefix_rejects_newline() {
        assert!(validate_prefix("P\n1").is_err());
    }

    // -----------------------------------------------------------------------
    // Discriminator 1 — underscore in prefix must be escaped (AC#8)
    //
    // A naive implementation using format!("{prefix}-%") without escaping
    // would produce "P_1-%" for prefix "P_1". In a SQLite LIKE clause this
    // would match "PA1-US-001", "PB1-US-001", etc. (any single char in place
    // of the underscore). The escaped form "P\_1-%" only matches a literal
    // underscore.
    // -----------------------------------------------------------------------

    #[test]
    fn test_discriminator_underscore_escape_prevents_wildcard_match() {
        let escaped = escape_like("P_1");
        // Naive unescaped form
        let naive_pattern = format!("P_1-%");
        // Correctly escaped form
        let safe_pattern = format!("{escaped}-%");

        assert_eq!(escaped, "P\\_1", "underscore must be escaped to \\_");
        assert_eq!(safe_pattern, "P\\_1-%");
        // The naive and safe patterns differ — proving a naive impl is wrong.
        assert_ne!(
            naive_pattern, safe_pattern,
            "naive pattern is indistinguishable from safe pattern only when there is no underscore"
        );
    }

    // -----------------------------------------------------------------------
    // Discriminator 2 — dash separator prevents cross-prefix contamination (AC#9)
    //
    // When a user queries tasks for prefix "P1", only tasks whose IDs begin
    // with "P1-" should be returned. Without the literal "-" in the pattern a
    // query for prefix "P1" could match tasks from prefix "P10", "P11", etc.
    //
    // KNOWN LIMITATION: the pattern "P1-%" still matches task IDs that belong
    // to a different PRD whose prefix happens to start with "P1-" (e.g.,
    // prefix "P1-extra" → task "P1-extra-US-001"). Users should ensure their
    // PRD prefixes are distinct enough that this ambiguity does not arise.
    // prefix_where does not attempt to prevent this case.
    // -----------------------------------------------------------------------

    #[test]
    fn test_discriminator_dash_separator_excludes_numeric_suffix_prefixes() {
        let (_, param) = prefix_where(Some("P1"));
        let pattern = param.unwrap();

        // The pattern MUST include the literal dash so "P10-…" is NOT matched.
        assert!(
            pattern.contains("P1-"),
            "pattern must use 'P1-' with dash separator, not bare 'P1'"
        );

        // Specifically, a bare "P1%" pattern would be wrong:
        // "P1" + "%" would match "P10-US-001" and "P1extra-US-001".
        // "P1-" + "%" correctly excludes "P10-US-001" and "P1extra-US-001".
        assert!(
            !pattern.starts_with("P1%"),
            "pattern must NOT be 'P1%' — it must use a dash separator"
        );
    }
}
