//! Database module - connection and schema management.
//!
//! Provides SQLite database functionality for task-mgr including:
//! - Connection management with proper pragmas
//! - Schema creation and migrations
//! - Lockfile management for concurrency control

pub mod connection;
pub mod lock;
pub mod migrations;
pub mod prefix;
pub mod schema;

pub use connection::{open_and_migrate, open_connection};
pub use lock::LockGuard;
pub use migrations::{
    get_migration_status, get_schema_version, migrate_down, migrate_up, run_migrations,
    MigrationResult, MigrationStatus, CURRENT_SCHEMA_VERSION,
};
pub use schema::create_schema;
