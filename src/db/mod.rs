//! Database module - connection and schema management.
//!
//! Provides SQLite database functionality for task-mgr including:
//! - Connection management with proper pragmas
//! - Schema creation and migrations
//! - Lockfile management for concurrency control

pub mod connection;
pub mod lock;
pub mod migrations;
pub mod path;
pub mod prefix;
pub mod schema;
pub mod soft_archive;

pub use connection::{open_and_migrate, open_connection};
pub use lock::LockGuard;
pub use migrations::{
    CURRENT_SCHEMA_VERSION, MigrationResult, MigrationStatus, get_migration_status,
    get_schema_version, migrate_down, migrate_up, run_migrations,
};
pub use path::{DbDirSource, ResolvedDbDir, resolve_db_dir};
pub use schema::create_schema;
