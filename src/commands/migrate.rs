//! Migrate command for manual schema migration control.
//!
//! Provides commands to view migration status and manually apply/revert migrations.

use rusqlite::Connection;
use serde::Serialize;

use crate::TaskMgrResult;
use crate::db::{
    MigrationResult, MigrationStatus, get_migration_status, migrate_down, migrate_up,
    run_migrations,
};

/// Result of migration status command
#[derive(Debug, Clone, Serialize)]
pub struct StatusResult {
    pub current_version: i64,
    pub target_version: i64,
    pub pending_count: usize,
    pub pending: Vec<MigrationInfo>,
    pub applied: Vec<MigrationInfo>,
}

/// Information about a single migration
#[derive(Debug, Clone, Serialize)]
pub struct MigrationInfo {
    pub version: i64,
    pub description: String,
}

/// Result of migrate up/down/all commands
#[derive(Debug, Clone, Serialize)]
pub struct MigrateResult {
    pub from_version: i64,
    pub to_version: i64,
    pub applied: Vec<MigrationInfo>,
    pub success: bool,
}

impl From<MigrationStatus> for StatusResult {
    fn from(status: MigrationStatus) -> Self {
        StatusResult {
            current_version: status.current_version,
            target_version: status.target_version,
            pending_count: status.pending_count,
            pending: status
                .pending
                .into_iter()
                .map(|(v, d)| MigrationInfo {
                    version: v,
                    description: d,
                })
                .collect(),
            applied: status
                .applied
                .into_iter()
                .map(|(v, d)| MigrationInfo {
                    version: v,
                    description: d,
                })
                .collect(),
        }
    }
}

impl From<MigrationResult> for MigrateResult {
    fn from(result: MigrationResult) -> Self {
        MigrateResult {
            from_version: result.from_version,
            to_version: result.to_version,
            applied: result
                .applied
                .into_iter()
                .map(|(v, d)| MigrationInfo {
                    version: v,
                    description: d,
                })
                .collect(),
            success: true,
        }
    }
}

/// Get migration status without making changes
pub fn status(conn: &Connection) -> TaskMgrResult<StatusResult> {
    let status = get_migration_status(conn)?;
    Ok(status.into())
}

/// Apply the next pending migration
pub fn up(conn: &mut Connection) -> TaskMgrResult<MigrateResult> {
    let result = migrate_up(conn)?;
    Ok(result.into())
}

/// Revert the most recent migration
pub fn down(conn: &mut Connection) -> TaskMgrResult<MigrateResult> {
    let result = migrate_down(conn)?;
    Ok(result.into())
}

/// Apply all pending migrations
pub fn all(conn: &mut Connection) -> TaskMgrResult<MigrateResult> {
    let result = run_migrations(conn)?;
    Ok(result.into())
}

/// Format status result as text
pub fn format_status_text(result: &StatusResult) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "Schema Version: {} / {} (target)\n",
        result.current_version, result.target_version
    ));

    if result.current_version == result.target_version {
        output.push_str("Status: Up to date\n");
    } else {
        output.push_str(&format!(
            "Status: {} migration(s) pending\n",
            result.pending_count
        ));
    }

    if !result.applied.is_empty() {
        output.push_str("\nApplied migrations:\n");
        for m in &result.applied {
            output.push_str(&format!("  [v{}] {}\n", m.version, m.description));
        }
    }

    if !result.pending.is_empty() {
        output.push_str("\nPending migrations:\n");
        for m in &result.pending {
            output.push_str(&format!("  [v{}] {}\n", m.version, m.description));
        }
    }

    output
}

/// Format migrate result as text
pub fn format_migrate_text(result: &MigrateResult, action: &str) -> String {
    let mut output = String::new();

    if result.applied.is_empty() {
        match action {
            "up" => output.push_str("No pending migrations to apply.\n"),
            "down" => output.push_str("No migrations to revert (at version 0).\n"),
            "all" => output.push_str("Database is already at the latest version.\n"),
            _ => output.push_str("No changes made.\n"),
        }
    } else {
        output.push_str(&format!(
            "Migrated from version {} to version {}\n",
            result.from_version, result.to_version
        ));

        output.push_str("\nApplied:\n");
        for m in &result.applied {
            output.push_str(&format!("  [v{}] {}\n", m.version, m.description));
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CURRENT_SCHEMA_VERSION, create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    #[test]
    fn test_status_fresh_db() {
        let (_temp_dir, conn) = setup_db();

        let result = status(&conn).unwrap();

        assert_eq!(result.current_version, 0);
        assert_eq!(result.target_version, CURRENT_SCHEMA_VERSION);
        assert!(result.pending_count > 0);
        assert!(result.applied.is_empty());
    }

    #[test]
    fn test_status_after_migrations() {
        let (_temp_dir, mut conn) = setup_db();

        // Apply all migrations
        all(&mut conn).unwrap();

        let result = status(&conn).unwrap();

        assert_eq!(result.current_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(result.target_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(result.pending_count, 0);
        assert!(!result.applied.is_empty());
    }

    #[test]
    fn test_up_single_migration() {
        let (_temp_dir, mut conn) = setup_db();

        let result = up(&mut conn).unwrap();

        assert_eq!(result.from_version, 0);
        assert_eq!(result.to_version, 1);
        assert_eq!(result.applied.len(), 1);
        assert!(result.success);
    }

    #[test]
    fn test_down_reverts_migration() {
        let (_temp_dir, mut conn) = setup_db();

        // First apply
        all(&mut conn).unwrap();

        // Then revert
        let result = down(&mut conn).unwrap();

        assert_eq!(result.from_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(result.to_version, CURRENT_SCHEMA_VERSION - 1);
        assert_eq!(result.applied.len(), 1);
    }

    #[test]
    fn test_all_applies_all_pending() {
        let (_temp_dir, mut conn) = setup_db();

        let result = all(&mut conn).unwrap();

        assert_eq!(result.from_version, 0);
        assert_eq!(result.to_version, CURRENT_SCHEMA_VERSION);
        assert!(!result.applied.is_empty());
    }

    #[test]
    fn test_format_status_text() {
        let result = StatusResult {
            current_version: 0,
            target_version: 2,
            pending_count: 2,
            pending: vec![
                MigrationInfo {
                    version: 1,
                    description: "First migration".to_string(),
                },
                MigrationInfo {
                    version: 2,
                    description: "Second migration".to_string(),
                },
            ],
            applied: vec![],
        };

        let text = format_status_text(&result);

        assert!(text.contains("Schema Version: 0 / 2"));
        assert!(text.contains("2 migration(s) pending"));
        assert!(text.contains("[v1] First migration"));
        assert!(text.contains("[v2] Second migration"));
    }

    #[test]
    fn test_format_migrate_text_with_changes() {
        let result = MigrateResult {
            from_version: 0,
            to_version: 1,
            applied: vec![MigrationInfo {
                version: 1,
                description: "Test migration".to_string(),
            }],
            success: true,
        };

        let text = format_migrate_text(&result, "up");

        assert!(text.contains("Migrated from version 0 to version 1"));
        assert!(text.contains("[v1] Test migration"));
    }

    #[test]
    fn test_format_migrate_text_no_changes() {
        let result = MigrateResult {
            from_version: 1,
            to_version: 1,
            applied: vec![],
            success: true,
        };

        let text = format_migrate_text(&result, "up");

        assert!(text.contains("No pending migrations"));
    }
}
