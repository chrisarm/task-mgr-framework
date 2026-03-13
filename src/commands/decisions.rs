//! Decisions command implementation.
//!
//! Provides list/resolve/decline/revert operations on key architectural decisions.

use rusqlite::Connection;
use serde::Serialize;

use crate::db::schema::key_decisions as key_decisions_db;
use crate::loop_engine::config::KeyDecisionOption;
use crate::{TaskMgrError, TaskMgrResult};

/// Summary of a key decision for display.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionSummary {
    pub id: i64,
    pub title: String,
    pub status: String,
    pub options: Vec<KeyDecisionOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

/// Result of listing key decisions.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionsListResult {
    pub decisions: Vec<DecisionSummary>,
    pub showing_all: bool,
}

/// Result of resolving a key decision.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionResolveResult {
    pub id: i64,
    pub title: String,
    pub resolution: String,
}

/// Result of declining a key decision.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionDeclineResult {
    pub id: i64,
    pub title: String,
    pub resolution: String,
}

/// Result of reverting a key decision to pending.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionRevertResult {
    pub id: i64,
    pub title: String,
}

/// List key decisions.
///
/// # Arguments
/// * `conn` - Database connection
/// * `all` - If false (default), shows only pending/deferred. If true, shows all.
pub fn list_decisions(conn: &Connection, all: bool) -> TaskMgrResult<DecisionsListResult> {
    let stored = if all {
        key_decisions_db::get_all_decisions(conn, None)?
    } else {
        key_decisions_db::get_all_pending_decisions(conn)?
    };

    let decisions = stored
        .into_iter()
        .map(|d| DecisionSummary {
            id: d.id,
            title: d.title,
            status: d.status,
            options: d.options,
            resolution: d.resolution,
            resolved_at: d.resolved_at,
        })
        .collect();

    Ok(DecisionsListResult {
        decisions,
        showing_all: all,
    })
}

/// Resolve a key decision by selecting an option.
///
/// # Arguments
/// * `conn` - Database connection
/// * `id` - Decision ID
/// * `option_str` - Letter (a/b/..., case-insensitive) or label substring
///
/// # Errors
/// * `NotFound` if decision ID doesn't exist or option letter is out of range
/// * `InvalidState` if the decision is already resolved
pub fn resolve_decision_cmd(
    conn: &Connection,
    id: i64,
    option_str: &str,
) -> TaskMgrResult<DecisionResolveResult> {
    let decision = key_decisions_db::get_decision_by_id(conn, id)?
        .ok_or_else(|| TaskMgrError::decision_not_found(id.to_string()))?;

    if decision.status == "resolved" {
        return Err(TaskMgrError::invalid_state(
            "Key Decision",
            id.to_string(),
            "pending or deferred",
            "resolved",
        ));
    }

    let chosen_opt = find_option(&decision.options, option_str)?;
    let resolution = format!("{}: {}", chosen_opt.label, chosen_opt.description);

    key_decisions_db::resolve_decision(conn, id, &resolution)?;

    Ok(DecisionResolveResult {
        id,
        title: decision.title,
        resolution,
    })
}

/// Decline a key decision (mark as not needed).
///
/// # Arguments
/// * `conn` - Database connection
/// * `id` - Decision ID
/// * `reason` - Optional reason; defaults to "Not needed"
///
/// # Errors
/// * `NotFound` if decision ID doesn't exist
/// * `InvalidState` if the decision is already resolved
pub fn decline_decision_cmd(
    conn: &Connection,
    id: i64,
    reason: Option<&str>,
) -> TaskMgrResult<DecisionDeclineResult> {
    let decision = key_decisions_db::get_decision_by_id(conn, id)?
        .ok_or_else(|| TaskMgrError::decision_not_found(id.to_string()))?;

    if decision.status == "resolved" {
        return Err(TaskMgrError::invalid_state(
            "Key Decision",
            id.to_string(),
            "pending or deferred",
            "resolved",
        ));
    }

    let resolution = format!("DECLINED: {}", reason.unwrap_or("Not needed"));
    key_decisions_db::resolve_decision(conn, id, &resolution)?;

    Ok(DecisionDeclineResult {
        id,
        title: decision.title,
        resolution,
    })
}

/// Revert a resolved or deferred decision back to pending.
///
/// # Arguments
/// * `conn` - Database connection
/// * `id` - Decision ID
///
/// # Errors
/// * `NotFound` if decision ID doesn't exist
/// * `InvalidState` if the decision is already pending
pub fn revert_decision_cmd(conn: &Connection, id: i64) -> TaskMgrResult<DecisionRevertResult> {
    // Fetch title before revert (revert_decision doesn't return the title)
    let decision = key_decisions_db::get_decision_by_id(conn, id)?
        .ok_or_else(|| TaskMgrError::decision_not_found(id.to_string()))?;

    let title = decision.title.clone();

    // revert_decision handles pending→InvalidState and missing→NotFound
    key_decisions_db::revert_decision(conn, id)?;

    Ok(DecisionRevertResult { id, title })
}

/// Map a single ASCII letter ('a'/'A' → 0, 'b'/'B' → 1, …) to the matching option.
///
/// Returns `NotFound` with a descriptive message if the letter exceeds the number of options.
fn letter_to_option(options: &[KeyDecisionOption], ch: char) -> TaskMgrResult<&KeyDecisionOption> {
    let idx = (ch.to_ascii_lowercase() as u8).wrapping_sub(b'a') as usize;
    options.get(idx).ok_or_else(|| {
        let max_letter = (b'A' + options.len().saturating_sub(1) as u8) as char;
        TaskMgrError::NotFound {
            resource_type: "Option".to_string(),
            id: format!(
                "'{}' — valid options are A–{} (this decision has {} option(s))",
                ch.to_ascii_uppercase(),
                max_letter,
                options.len()
            ),
        }
    })
}

/// Find an option by single-letter index or label substring.
///
/// Letter matching: 'a'/'A' → index 0, 'b'/'B' → index 1, etc. (case-insensitive).
/// Returns `NotFound` if the letter is out of range or no label matches the substring.
pub(crate) fn find_option<'a>(
    options: &'a [KeyDecisionOption],
    option_str: &str,
) -> TaskMgrResult<&'a KeyDecisionOption> {
    let trimmed = option_str.trim();

    // Try single letter mapping: a → 0, b → 1, …
    if trimmed.len() == 1 {
        let ch = trimmed.chars().next().unwrap(); // safe: len == 1
        if ch.is_ascii_alphabetic() {
            return letter_to_option(options, ch);
        }
    }

    // Fall back to case-insensitive label substring match
    let lower = trimmed.to_lowercase();
    options
        .iter()
        .find(|opt| opt.label.to_lowercase().contains(&lower))
        .ok_or_else(|| TaskMgrError::NotFound {
            resource_type: "Option".to_string(),
            id: format!("'{}' did not match any option label", trimmed),
        })
}

/// Format list result as human-readable text.
#[must_use]
pub fn format_list_text(result: &DecisionsListResult) -> String {
    if result.decisions.is_empty() {
        return if result.showing_all {
            "No key decisions recorded.\n".to_string()
        } else {
            "No pending key decisions.\n".to_string()
        };
    }

    let header = if result.showing_all {
        "All key decisions"
    } else {
        "Pending key decisions"
    };

    let mut out = format!("{} ({}):\n", header, result.decisions.len());
    for d in &result.decisions {
        out.push_str(&format!(
            "  [{}] [{}] {} ({} option(s))\n",
            d.id,
            d.status.to_uppercase(),
            d.title,
            d.options.len()
        ));
        if let Some(ref res) = d.resolution {
            out.push_str(&format!("       → {}\n", res));
        }
    }
    out
}

/// Format resolve result as human-readable text.
#[must_use]
pub fn format_resolve_text(result: &DecisionResolveResult) -> String {
    format!(
        "Resolved decision #{}: {}\n  → {}\n",
        result.id, result.title, result.resolution
    )
}

/// Format decline result as human-readable text.
#[must_use]
pub fn format_decline_text(result: &DecisionDeclineResult) -> String {
    format!(
        "Declined decision #{}: {}\n  → {}\n",
        result.id, result.title, result.resolution
    )
}

/// Format revert result as human-readable text.
#[must_use]
pub fn format_revert_text(result: &DecisionRevertResult) -> String {
    format!(
        "Reverted decision #{} '{}' back to pending.\n",
        result.id, result.title
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::schema::key_decisions as key_decisions_db;
    use crate::db::{create_schema, open_connection};
    use crate::loop_engine::config::{KeyDecision, KeyDecisionOption};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let mut conn = open_connection(dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
            .unwrap();
        (dir, conn)
    }

    fn make_decision() -> KeyDecision {
        KeyDecision {
            title: "Storage backend".to_string(),
            description: "Choose storage".to_string(),
            options: vec![
                KeyDecisionOption {
                    label: "SQLite".to_string(),
                    description: "Embedded".to_string(),
                },
                KeyDecisionOption {
                    label: "PostgreSQL".to_string(),
                    description: "Scalable".to_string(),
                },
            ],
        }
    }

    #[test]
    fn test_resolve_with_lowercase_a_picks_first_option() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        let result = resolve_decision_cmd(&conn, id, "a").unwrap();
        assert_eq!(result.resolution, "SQLite: Embedded");
    }

    #[test]
    fn test_resolve_with_uppercase_b_picks_second_option() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        let result = resolve_decision_cmd(&conn, id, "B").unwrap();
        assert_eq!(result.resolution, "PostgreSQL: Scalable");
    }

    #[test]
    fn test_resolve_on_resolved_decision_returns_invalid_state() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();
        key_decisions_db::resolve_decision(&conn, id, "Already done").unwrap();

        let err = resolve_decision_cmd(&conn, id, "a").unwrap_err();
        assert!(
            matches!(err, TaskMgrError::InvalidState { .. }),
            "expected InvalidState, got {err:?}"
        );
    }

    #[test]
    fn test_resolve_out_of_range_letter_returns_not_found() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        let err = resolve_decision_cmd(&conn, id, "Z").unwrap_err();
        assert!(
            matches!(err, TaskMgrError::NotFound { .. }),
            "expected NotFound, got {err:?}"
        );
    }

    #[test]
    fn test_decline_defaults_to_not_needed() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        let result = decline_decision_cmd(&conn, id, None).unwrap();
        assert_eq!(result.resolution, "DECLINED: Not needed");
    }

    #[test]
    fn test_decline_with_reason() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        let result = decline_decision_cmd(&conn, id, Some("Not MVP")).unwrap();
        assert_eq!(result.resolution, "DECLINED: Not MVP");
    }

    #[test]
    fn test_revert_resolved_decision_succeeds() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();
        key_decisions_db::resolve_decision(&conn, id, "Chosen").unwrap();

        let result = revert_decision_cmd(&conn, id).unwrap();
        assert_eq!(result.id, id);
        assert_eq!(result.title, "Storage backend");
    }

    #[test]
    fn test_revert_pending_decision_returns_invalid_state() {
        let (_dir, conn) = setup_db();
        let d = make_decision();
        let id = key_decisions_db::insert_key_decision(&conn, "run-001", None, 1, &d).unwrap();

        // Still pending — should fail with InvalidState
        let err = revert_decision_cmd(&conn, id).unwrap_err();
        assert!(
            matches!(err, TaskMgrError::InvalidState { .. }),
            "expected InvalidState, got {err:?}"
        );
    }

    #[test]
    fn test_format_list_text_empty_pending() {
        let result = DecisionsListResult {
            decisions: vec![],
            showing_all: false,
        };
        let text = format_list_text(&result);
        assert!(text.contains("No pending key decisions"));
    }

    #[test]
    fn test_format_list_text_with_decisions() {
        let result = DecisionsListResult {
            decisions: vec![DecisionSummary {
                id: 3,
                title: "Pick DB".to_string(),
                status: "pending".to_string(),
                options: vec![
                    KeyDecisionOption {
                        label: "A".to_string(),
                        description: "desc".to_string(),
                    },
                    KeyDecisionOption {
                        label: "B".to_string(),
                        description: "desc".to_string(),
                    },
                ],
                resolution: None,
                resolved_at: None,
            }],
            showing_all: false,
        };
        let text = format_list_text(&result);
        assert!(text.contains("[3]"), "expected ID [3] in: {text}");
        assert!(
            text.contains("PENDING"),
            "expected status PENDING in: {text}"
        );
        assert!(text.contains("Pick DB"), "expected title in: {text}");
        assert!(
            text.contains("2 option(s)"),
            "expected options count in: {text}"
        );
    }
}
