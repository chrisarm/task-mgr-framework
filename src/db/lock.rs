//! Lockfile management for exclusive database access.
//!
//! Ensures only one task-mgr instance runs per worktree using exclusive file locking.
//! This prevents concurrent corruption of the SQLite database.
//!
//! Two lock types are supported:
//! - `acquire()` — short-lived per-command lock (`tasks.db.lock`)
//! - `acquire_named()` — long-lived named lock (e.g. `loop.lock` held for hours)

use crate::error::{TaskMgrError, TaskMgrResult};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A guard that holds an exclusive lock on the task-mgr database.
///
/// When dropped, the lock is released and the lockfile is removed.
/// Only one `LockGuard` can exist per database directory at a time.
#[derive(Debug)]
pub struct LockGuard {
    /// The open file handle with the lock
    file: File,
    /// Path to the lockfile (for removal on drop)
    path: PathBuf,
}

impl LockGuard {
    /// Attempts to acquire an exclusive lock on the database directory.
    ///
    /// Creates a lockfile at `{dir}/tasks.db.lock` and acquires an exclusive lock.
    /// If the lock is already held by another process, returns an error with the
    /// holder's identity (if available).
    ///
    /// # Arguments
    ///
    /// * `dir` - The database directory (typically `.task-mgr/`)
    ///
    /// # Errors
    ///
    /// Returns `TaskMgrError::LockError` if:
    /// - The lock is already held by another process
    /// - Unable to create/open the lockfile
    /// - Unable to write holder info to the lockfile
    pub fn acquire(dir: impl AsRef<Path>) -> TaskMgrResult<Self> {
        Self::acquire_inner(dir.as_ref(), "tasks.db.lock")
    }

    /// Like `acquire()` but uses a custom lock file name.
    ///
    /// Advisory file locks (flock) protect against concurrent processes on the
    /// same kernel only. They do NOT provide cross-machine protection on
    /// network/cloud-synced filesystems (e.g., Dropbox, NFS).
    ///
    /// The holder's `{pid}@{hostname}` is written to the lock file so
    /// cross-machine conflicts are diagnosable.
    ///
    /// # Arguments
    ///
    /// * `dir` - The database directory (typically `.task-mgr/`)
    /// * `filename` - Lock file name (e.g. `"loop.lock"`)
    pub fn acquire_named(dir: impl AsRef<Path>, filename: &str) -> TaskMgrResult<Self> {
        Self::acquire_inner(dir.as_ref(), filename)
    }

    /// Shared implementation for `acquire()` and `acquire_named()`.
    fn acquire_inner(dir: &Path, filename: &str) -> TaskMgrResult<Self> {
        let lock_path = dir.join(filename);

        // Ensure directory exists
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }

        // Try to read existing holder info before attempting lock
        let existing_holder = Self::read_holder_info(&lock_path);

        // Open or create the lockfile
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        // Try to acquire exclusive lock (non-blocking)
        match file.try_lock_exclusive() {
            Ok(()) => {
                // Successfully acquired lock, write our identity
                let mut guard = LockGuard {
                    file,
                    path: lock_path,
                };
                guard.write_holder_info()?;
                Ok(guard)
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                // Lock is held by another process
                let message = if let Some(holder) = existing_holder {
                    format!("Database is locked by another process ({})", holder)
                } else {
                    "Database is locked by another process".to_string()
                };
                Err(TaskMgrError::lock_error(message))
            }
            Err(err) => {
                // Other error (e.g., permission denied)
                Err(TaskMgrError::lock_error(format!(
                    "Failed to acquire lock: {}",
                    err
                )))
            }
        }
    }

    /// Writes the current process identity (`{pid}@{hostname}`) to the lockfile.
    fn write_holder_info(&mut self) -> TaskMgrResult<()> {
        self.file.set_len(0)?;
        let pid = std::process::id();
        let host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());
        write!(self.file, "{}@{}", pid, host)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Reads the holder identity string from an existing lockfile, if present.
    ///
    /// Returns the raw contents (e.g. `"12345@myhost"`) or `None` if the file
    /// doesn't exist or can't be read.
    fn read_holder_info(path: &Path) -> Option<String> {
        let mut file = File::open(path).ok()?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).ok()?;
        let trimmed = contents.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Returns the path to the lockfile.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Release the lock
        let _ = self.file.unlock();
        // Remove the lockfile (best effort)
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_creates_lockfile() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("tasks.db.lock");

        // Lock file shouldn't exist yet
        assert!(!lock_path.exists());

        // Acquire lock
        let guard = LockGuard::acquire(temp_dir.path()).unwrap();

        // Lock file should exist now
        assert!(lock_path.exists());

        // Verify guard's path is correct
        assert_eq!(guard.path(), lock_path);
    }

    #[test]
    fn test_acquire_writes_pid_and_hostname() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("tasks.db.lock");

        let _guard = LockGuard::acquire(temp_dir.path()).unwrap();

        // Read the holder info from the lockfile
        let contents = fs::read_to_string(&lock_path).unwrap();

        // Should contain our PID
        let our_pid = std::process::id().to_string();
        assert!(
            contents.contains(&our_pid),
            "Lock file should contain PID {}: {}",
            our_pid,
            contents
        );

        // Should contain @ separator and hostname
        assert!(
            contents.contains('@'),
            "Lock file should contain pid@hostname format: {}",
            contents
        );
    }

    #[test]
    fn test_acquire_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let nested_dir = temp_dir.path().join("nested").join("dir");

        // Directory shouldn't exist yet
        assert!(!nested_dir.exists());

        // Acquire lock should create directory
        let _guard = LockGuard::acquire(&nested_dir).unwrap();

        // Directory and lockfile should exist now
        assert!(nested_dir.exists());
        assert!(nested_dir.join("tasks.db.lock").exists());
    }

    #[test]
    fn test_second_acquire_fails_while_lock_held() {
        let temp_dir = TempDir::new().unwrap();

        // First acquire succeeds
        let _guard1 = LockGuard::acquire(temp_dir.path()).unwrap();

        // Second acquire should fail
        let result = LockGuard::acquire(temp_dir.path());
        assert!(result.is_err());

        // Error message should mention lock
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("locked"), "Error should mention lock: {}", msg);
    }

    #[test]
    fn test_second_acquire_shows_holder_pid() {
        let temp_dir = TempDir::new().unwrap();

        // First acquire succeeds
        let _guard1 = LockGuard::acquire(temp_dir.path()).unwrap();
        let our_pid = std::process::id();

        // Second acquire should fail with PID in message
        let result = LockGuard::acquire(temp_dir.path());
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&our_pid.to_string()),
            "Error should contain PID {}: {}",
            our_pid,
            msg
        );
    }

    #[test]
    fn test_lock_released_on_drop() {
        let temp_dir = TempDir::new().unwrap();

        // Acquire and drop lock
        {
            let _guard = LockGuard::acquire(temp_dir.path()).unwrap();
            // Lock is held here
        }
        // Lock should be released now

        // Should be able to acquire again
        let result = LockGuard::acquire(temp_dir.path());
        assert!(result.is_ok(), "Should be able to acquire after drop");
    }

    #[test]
    fn test_lockfile_removed_on_drop() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("tasks.db.lock");

        // Acquire and drop lock
        {
            let _guard = LockGuard::acquire(temp_dir.path()).unwrap();
            assert!(lock_path.exists(), "Lockfile should exist while held");
        }

        // Lockfile should be removed after drop
        assert!(!lock_path.exists(), "Lockfile should be removed after drop");
    }

    #[test]
    fn test_read_holder_info_returns_none_for_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("nonexistent.lock");

        let info = LockGuard::read_holder_info(&lock_path);
        assert!(info.is_none());
    }

    #[test]
    fn test_read_holder_info_returns_none_for_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("empty.lock");

        fs::write(&lock_path, "").unwrap();

        let info = LockGuard::read_holder_info(&lock_path);
        assert!(info.is_none());
    }

    #[test]
    fn test_read_holder_info_reads_pid_at_hostname() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("valid.lock");

        fs::write(&lock_path, "12345@myhost").unwrap();

        let info = LockGuard::read_holder_info(&lock_path);
        assert_eq!(info, Some("12345@myhost".to_string()));
    }

    #[test]
    fn test_read_holder_info_handles_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("whitespace.lock");

        fs::write(&lock_path, "67890@host\n").unwrap();

        let info = LockGuard::read_holder_info(&lock_path);
        assert_eq!(info, Some("67890@host".to_string()));
    }

    #[test]
    fn test_lock_guard_path_returns_correct_path() {
        let temp_dir = TempDir::new().unwrap();
        let expected_path = temp_dir.path().join("tasks.db.lock");

        let guard = LockGuard::acquire(temp_dir.path()).unwrap();
        assert_eq!(guard.path(), expected_path);
    }

    // --- acquire_named tests ---

    #[test]
    fn test_acquire_named_uses_custom_filename() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("loop.lock");

        assert!(!lock_path.exists());

        let guard = LockGuard::acquire_named(temp_dir.path(), "loop.lock").unwrap();

        assert!(lock_path.exists());
        assert_eq!(guard.path(), lock_path);
    }

    #[test]
    fn test_acquire_named_blocks_concurrent() {
        let temp_dir = TempDir::new().unwrap();

        // First acquire_named succeeds
        let _guard1 = LockGuard::acquire_named(temp_dir.path(), "loop.lock").unwrap();

        // Second acquire_named with same name fails
        let result = LockGuard::acquire_named(temp_dir.path(), "loop.lock");
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("locked"), "Error should mention lock: {}", msg);
    }

    #[test]
    fn test_acquire_named_independent_of_acquire() {
        let temp_dir = TempDir::new().unwrap();

        // acquire() and acquire_named("loop.lock") use different files — no conflict
        let _guard1 = LockGuard::acquire(temp_dir.path()).unwrap();
        let _guard2 = LockGuard::acquire_named(temp_dir.path(), "loop.lock").unwrap();

        // Both should coexist
        assert!(temp_dir.path().join("tasks.db.lock").exists());
        assert!(temp_dir.path().join("loop.lock").exists());
    }

    #[test]
    fn test_stale_lockfile_does_not_block() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("loop.lock");

        // Create a stale lock file with PID written but no flock held
        // (simulates post-SIGKILL state where OS released the flock but file remains)
        fs::write(&lock_path, "99999@deadhost").unwrap();

        // acquire_named should succeed because the kernel flock is not held
        let guard = LockGuard::acquire_named(temp_dir.path(), "loop.lock").unwrap();
        assert_eq!(guard.path(), lock_path);

        // Should have overwritten with our info
        let contents = fs::read_to_string(&lock_path).unwrap();
        assert!(contents.contains(&std::process::id().to_string()));
    }
}
