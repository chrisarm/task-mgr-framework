//! Database connection management for task-mgr.
//!
//! Provides SQLite connection with proper pragmas for reliability:
//! - WAL mode for crash recovery
//! - Foreign key enforcement
//! - Appropriate cache and timeout settings
//!
//! # Connection Pooling Analysis (PERF-001)
//!
//! Connection pooling (e.g., r2d2) was evaluated and determined to be NOT BENEFICIAL
//! for this CLI tool. Here's the analysis:
//!
//! ## Why Pooling Doesn't Help CLIs
//!
//! 1. **Process Lifetime**: Each CLI invocation is a separate process. Connection pools
//!    exist in process memory and are destroyed when the process exits. There's no
//!    opportunity to reuse connections across invocations.
//!
//! 2. **No Network Overhead**: SQLite uses local file I/O, not network connections.
//!    Pooling primarily helps with network databases (PostgreSQL, MySQL) where
//!    connection establishment involves TCP handshakes, authentication, etc.
//!
//! 3. **Already Fast**: Measured latency shows commands complete in 2-4ms total,
//!    with connection opening taking ~1ms. There's no meaningful overhead to optimize.
//!
//! ## Benchmarks (release build, 7-task PRD)
//!
//! - `list` command: ~3ms average
//! - `next` command: ~4ms average
//! - `stats` command: ~3ms average
//! - 50 sequential invocations: 178ms total (~3.5ms/invocation)
//!
//! ## When Pooling WOULD Help
//!
//! - Long-lived daemon process handling multiple requests
//! - Remote database with network latency
//! - Multi-threaded application with concurrent DB access
//!
//! ## Conclusion
//!
//! The current single-connection-per-invocation architecture is optimal for CLI use.
//! WAL mode already provides excellent concurrent read performance. Adding pooling
//! would increase complexity without measurable benefit.

use std::path::Path;

use rusqlite::Connection;

use super::migrations::run_migrations;
use super::schema::create_schema;
use crate::TaskMgrResult;

/// Opens a SQLite connection and runs any pending migrations.
///
/// Convenience wrapper around [`open_connection`] + [`run_migrations`] for
/// production CLI commands. Tests that need to control migration state should
/// call `open_connection` directly instead.
pub fn open_and_migrate(db_dir: &Path) -> TaskMgrResult<Connection> {
    let mut conn = open_connection(db_dir)?;
    if let Err(e) = run_migrations(&mut conn) {
        eprintln!("Warning: failed to run migrations: {} (continuing)", e);
    }
    Ok(conn)
}

/// Opens a SQLite connection with appropriate pragmas set for reliability and performance.
///
/// # Arguments
///
/// * `db_dir` - Path to the directory containing the database file. The directory
///   will be created if it doesn't exist. The database file will be named `tasks.db`.
///
/// # Pragmas Set
///
/// - `journal_mode = WAL` - Write-Ahead Logging for crash recovery
/// - `synchronous = FULL` - Full durability guarantees
/// - `foreign_keys = ON` - Enforce referential integrity
/// - `busy_timeout = 5000` - Wait up to 5s for locks
/// - `cache_size = -64000` - 64MB page cache
/// - `temp_store = MEMORY` - Store temp tables in memory
///
/// # Errors
///
/// Returns an error if:
/// - The directory cannot be created
/// - The database file cannot be opened or created
/// - Any pragma fails to set
pub fn open_connection(db_dir: &Path) -> TaskMgrResult<Connection> {
    // Create directory if it doesn't exist
    if !db_dir.exists() {
        std::fs::create_dir_all(db_dir)?;
    }

    let db_path = db_dir.join("tasks.db");
    let conn = Connection::open(&db_path)?;

    // Set pragmas for reliability and performance
    // WAL mode enables crash recovery and better concurrent reads
    conn.pragma_update(None, "journal_mode", "WAL")?;

    // FULL synchronous ensures durability (data persisted on commit)
    conn.pragma_update(None, "synchronous", "FULL")?;

    // Enable foreign key enforcement
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // Wait up to 5 seconds for locks before returning SQLITE_BUSY
    conn.pragma_update(None, "busy_timeout", 5000)?;

    // 64MB page cache (negative value = KB, so -64000 = ~64MB)
    conn.pragma_update(None, "cache_size", -64000)?;

    // Store temporary tables and indices in memory
    conn.pragma_update(None, "temp_store", "MEMORY")?;

    // Ensure base schema exists (idempotent - uses CREATE TABLE IF NOT EXISTS)
    create_schema(&conn)?;

    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_open_connection_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let db_dir = temp_dir.path().join("new_subdir");

        assert!(!db_dir.exists());
        let _conn = open_connection(&db_dir).unwrap();
        assert!(db_dir.exists());
        assert!(db_dir.join("tasks.db").exists());
    }

    #[test]
    fn test_pragmas_are_set_correctly() {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();

        // Verify WAL mode
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");

        // Verify synchronous = FULL (value 2)
        let synchronous: i32 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 2); // FULL = 2

        // Verify foreign keys enabled
        let foreign_keys: i32 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);

        // Verify busy timeout
        let busy_timeout: i32 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 5000);

        // Verify cache size (negative = KB)
        let cache_size: i32 = conn
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap();
        assert_eq!(cache_size, -64000);

        // Verify temp_store = MEMORY (value 2)
        let temp_store: i32 = conn
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, 2); // MEMORY = 2
    }

    #[test]
    fn test_connection_can_execute_queries() {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();

        // Create a simple table and verify operations work
        conn.execute(
            "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            [],
        )
        .unwrap();

        conn.execute("INSERT INTO test (name) VALUES (?)", ["test_value"])
            .unwrap();

        let name: String = conn
            .query_row("SELECT name FROM test WHERE id = 1", [], |row| row.get(0))
            .unwrap();

        assert_eq!(name, "test_value");
    }

    #[test]
    fn test_foreign_keys_enforced() {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();

        // Create parent and child tables
        conn.execute(
            "CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))",
            [],
        )
        .unwrap();

        // Inserting a child with non-existent parent should fail
        let result = conn.execute("INSERT INTO child (parent_id) VALUES (999)", []);
        assert!(result.is_err());
    }

    #[test]
    fn test_reopen_existing_database() {
        let temp_dir = TempDir::new().unwrap();

        // Create and populate database
        {
            let conn = open_connection(temp_dir.path()).unwrap();
            conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY)", [])
                .unwrap();
            conn.execute("INSERT INTO test (id) VALUES (42)", [])
                .unwrap();
        }

        // Reopen and verify data persists
        {
            let conn = open_connection(temp_dir.path()).unwrap();
            let id: i32 = conn
                .query_row("SELECT id FROM test", [], |row| row.get(0))
                .unwrap();
            assert_eq!(id, 42);
        }
    }
}
