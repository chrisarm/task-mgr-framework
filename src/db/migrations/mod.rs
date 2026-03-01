//! Database schema migration support for task-mgr.
//!
//! Provides a simple, ordered migration system for schema upgrades:
//! - Tracks schema version in global_state table
//! - Migrations run automatically on db open if version mismatch
//! - Supports both up and down migrations
//! - Each migration runs in a transaction for atomicity

mod v1;
mod v2;
mod v3;
mod v4;
mod v5;
mod v6;
mod v7;
mod v8;
mod v9;

#[cfg(test)]
mod tests;

use rusqlite::{Connection, Transaction};

use crate::TaskMgrResult;

/// Current schema version - increment when adding new migrations
pub const CURRENT_SCHEMA_VERSION: i64 = 9;

/// A single migration with up and down SQL
pub struct Migration {
    /// Version number this migration upgrades TO
    pub version: i64,
    /// Description of what this migration does
    pub description: &'static str,
    /// SQL to apply the migration (upgrade)
    pub up_sql: &'static str,
    /// SQL to revert the migration (downgrade)
    pub down_sql: &'static str,
}

/// All migrations in order. Each migration upgrades from version-1 to version.
/// Version 0 is the initial schema from create_schema().
pub static MIGRATIONS: &[&Migration] = &[
    &v1::MIGRATION,
    &v2::MIGRATION,
    &v3::MIGRATION,
    &v4::MIGRATION,
    &v5::MIGRATION,
    &v6::MIGRATION,
    &v7::MIGRATION,
    &v8::MIGRATION,
    &v9::MIGRATION,
];

/// Get the current schema version from the database.
/// Returns 0 if schema_version column doesn't exist (pre-migration database).
pub fn get_schema_version(conn: &Connection) -> TaskMgrResult<i64> {
    // Check if schema_version column exists
    let column_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('global_state') WHERE name = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !column_exists {
        // Pre-migration database
        return Ok(0);
    }

    // Get the schema version
    let version: i64 = conn
        .query_row(
            "SELECT schema_version FROM global_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok(version)
}

/// Set the schema version in the database.
fn set_schema_version(tx: &Transaction<'_>, version: i64) -> TaskMgrResult<()> {
    tx.execute(
        "UPDATE global_state SET schema_version = ?, updated_at = datetime('now') WHERE id = 1",
        [version],
    )?;
    Ok(())
}

/// Result of running migrations
#[derive(Debug, Clone)]
pub struct MigrationResult {
    /// Starting version before migration
    pub from_version: i64,
    /// Ending version after migration
    pub to_version: i64,
    /// Migrations that were applied (version, description)
    pub applied: Vec<(i64, String)>,
}

/// Run all pending migrations to bring database to current version.
/// Each migration runs in its own transaction.
pub fn run_migrations(conn: &mut Connection) -> TaskMgrResult<MigrationResult> {
    let from_version = get_schema_version(conn)?;
    let mut applied = Vec::new();

    // Apply each migration in order
    for migration in MIGRATIONS.iter() {
        if migration.version > from_version {
            // Run this migration in a transaction
            let tx = conn.transaction()?;

            // Execute the up SQL (may contain multiple statements)
            tx.execute_batch(migration.up_sql)?;

            // For version 1, schema_version column was just created by the migration itself
            // For later versions, we need to update it
            if migration.version > 1 {
                set_schema_version(&tx, migration.version)?;
            }

            tx.commit()?;

            applied.push((migration.version, migration.description.to_string()));
        }
    }

    let to_version = if applied.is_empty() {
        from_version
    } else {
        applied.last().map(|(v, _)| *v).unwrap_or(from_version)
    };

    Ok(MigrationResult {
        from_version,
        to_version,
        applied,
    })
}

/// Run a single migration up (for manual control).
pub fn migrate_up(conn: &mut Connection) -> TaskMgrResult<MigrationResult> {
    let from_version = get_schema_version(conn)?;

    // Find next migration to apply
    let next_migration = MIGRATIONS.iter().find(|m| m.version > from_version);

    let mut applied = Vec::new();

    if let Some(migration) = next_migration {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.up_sql)?;

        if migration.version > 1 {
            set_schema_version(&tx, migration.version)?;
        }

        tx.commit()?;

        applied.push((migration.version, migration.description.to_string()));
    }

    let to_version = if applied.is_empty() {
        from_version
    } else {
        applied[0].0
    };

    Ok(MigrationResult {
        from_version,
        to_version,
        applied,
    })
}

/// Run a single migration down (for manual control).
pub fn migrate_down(conn: &mut Connection) -> TaskMgrResult<MigrationResult> {
    let from_version = get_schema_version(conn)?;

    if from_version == 0 {
        // Nothing to revert
        return Ok(MigrationResult {
            from_version,
            to_version: 0,
            applied: Vec::new(),
        });
    }

    // Find current migration to revert
    let current_migration = MIGRATIONS.iter().find(|m| m.version == from_version);

    let mut applied = Vec::new();

    if let Some(migration) = current_migration {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.down_sql)?;

        let new_version = migration.version - 1;

        // After reverting version 1, schema_version column won't exist anymore
        // So we only set version for downgrades from version > 1
        if new_version >= 1 {
            set_schema_version(&tx, new_version)?;
        }

        tx.commit()?;

        applied.push((
            migration.version,
            format!("Reverted: {}", migration.description),
        ));
    }

    let to_version = if applied.is_empty() {
        from_version
    } else {
        from_version - 1
    };

    Ok(MigrationResult {
        from_version,
        to_version,
        applied,
    })
}

/// Get migration status information
#[derive(Debug, Clone)]
pub struct MigrationStatus {
    /// Current schema version
    pub current_version: i64,
    /// Target schema version (latest migration)
    pub target_version: i64,
    /// Number of pending migrations
    pub pending_count: usize,
    /// Pending migrations (version, description)
    pub pending: Vec<(i64, String)>,
    /// Applied migrations (version, description)
    pub applied: Vec<(i64, String)>,
}

/// Get the current migration status without making changes.
pub fn get_migration_status(conn: &Connection) -> TaskMgrResult<MigrationStatus> {
    let current_version = get_schema_version(conn)?;
    let target_version = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);

    let mut pending = Vec::new();
    let mut applied = Vec::new();

    for migration in MIGRATIONS.iter() {
        if migration.version <= current_version {
            applied.push((migration.version, migration.description.to_string()));
        } else {
            pending.push((migration.version, migration.description.to_string()));
        }
    }

    Ok(MigrationStatus {
        current_version,
        target_version,
        pending_count: pending.len(),
        pending,
        applied,
    })
}
