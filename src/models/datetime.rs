//! Shared datetime parsing utilities for models.
//!
//! This module provides functions for parsing SQLite datetime strings
//! into chrono DateTime types.

use chrono::{DateTime, Utc};

use crate::TaskMgrError;

/// Parses a SQLite datetime string into a DateTime<Utc>.
///
/// SQLite uses ISO 8601 format: "YYYY-MM-DD HH:MM:SS" from `datetime('now')`.
///
/// # Errors
///
/// Returns `TaskMgrError::InvalidState` if the string is not a valid datetime.
pub fn parse_datetime(s: &str) -> Result<DateTime<Utc>, TaskMgrError> {
    let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").map_err(|e| {
        TaskMgrError::invalid_state("datetime", s, "ISO 8601 format", e.to_string())
    })?;
    Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
}

/// Parses an optional SQLite datetime string.
///
/// Returns `Ok(None)` if the input is `None`, otherwise parses the string.
///
/// # Errors
///
/// Returns `TaskMgrError::InvalidState` if the string is present but not a valid datetime.
pub fn parse_optional_datetime(s: Option<String>) -> Result<Option<DateTime<Utc>>, TaskMgrError> {
    match s {
        Some(s) => Ok(Some(parse_datetime(&s)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn test_parse_datetime() {
        let dt = parse_datetime("2026-01-18 12:30:45").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 18);
        assert_eq!(dt.hour(), 12);
        assert_eq!(dt.minute(), 30);
        assert_eq!(dt.second(), 45);
    }

    #[test]
    fn test_parse_datetime_invalid() {
        let result = parse_datetime("not a date");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_optional_datetime_some() {
        let result = parse_optional_datetime(Some("2026-01-18 12:30:45".to_string())).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().year(), 2026);
    }

    #[test]
    fn test_parse_optional_datetime_none() {
        let result = parse_optional_datetime(None).unwrap();
        assert!(result.is_none());
    }
}
