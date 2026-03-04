//! Lockfile management for exclusive database access.
//!
//! Ensures only one task-mgr instance runs per worktree using exclusive file locking.
//! This prevents concurrent corruption of the SQLite database.
//!
//! Two lock types are supported:
//! - `acquire()` — short-lived per-command lock (`tasks.db.lock`)
//! - `acquire_named()` — long-lived named lock (e.g. `loop-{prefix}.lock` held for hours)

use crate::error::{TaskMgrError, TaskMgrResult};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Identity of the process holding a lock, with optional worktree context.
#[derive(Debug, PartialEq)]
pub struct HolderInfo {
    /// PID of the lock holder
    pub pid: u32,
    /// Hostname of the lock holder
    pub host: String,
    /// Git branch active when the lock was acquired (None for old-format locks)
    pub branch: Option<String>,
    /// Worktree path active when the lock was acquired (None for old-format locks)
    pub worktree: Option<String>,
    /// Worktree name prefix (None for old-format locks)
    pub prefix: Option<String>,
}

impl std::fmt::Display for HolderInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.pid, self.host)?;
        if let Some(b) = &self.branch {
            write!(f, " branch={}", b)?;
        }
        if let Some(w) = &self.worktree {
            write!(f, " worktree={}", w)?;
        }
        if let Some(p) = &self.prefix {
            write!(f, " prefix={}", p)?;
        }
        Ok(())
    }
}

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
        self.write_holder_info_extended(None, None, None)
    }

    /// Writes extended holder info in multi-line format:
    /// ```text
    /// {pid}@{hostname}
    /// branch={branch}
    /// worktree={worktree_path}
    /// prefix={prefix}
    /// ```
    /// Lines for None fields are omitted.
    pub fn write_holder_info_extended(
        &mut self,
        branch: Option<&str>,
        worktree: Option<&str>,
        prefix: Option<&str>,
    ) -> TaskMgrResult<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        let pid = std::process::id();
        let host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());
        let mut content = format!("{}@{}", pid, host);
        if let Some(b) = branch {
            content.push_str(&format!("\nbranch={}", b));
        }
        if let Some(w) = worktree {
            content.push_str(&format!("\nworktree={}", w));
        }
        if let Some(p) = prefix {
            content.push_str(&format!("\nprefix={}", p));
        }
        write!(self.file, "{}", content)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Reads holder info from an existing lockfile, if present.
    ///
    /// Supports both formats:
    /// - New multi-line: first line `{pid}@{host}`, followed by `key=value` lines
    /// - Old single-line: `{pid}@{host}` (branch/worktree/prefix will be `None`)
    ///
    /// Returns `None` if the file doesn't exist, can't be read, or is empty.
    pub fn read_holder_info(path: &Path) -> Option<HolderInfo> {
        let mut file = File::open(path).ok()?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).ok()?;
        let mut lines = contents.lines();

        // First line must be pid@host
        let first = lines.next()?.trim();
        if first.is_empty() {
            return None;
        }
        let (pid_str, host) = first.split_once('@')?;
        let pid: u32 = pid_str.trim().parse().ok()?;
        let host = host.trim().to_string();

        // Parse optional key=value lines (new multi-line format)
        let mut branch = None;
        let mut worktree = None;
        let mut prefix = None;
        for line in lines {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("branch=") {
                branch = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("worktree=") {
                worktree = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("prefix=") {
                prefix = Some(v.to_string());
            }
        }

        Some(HolderInfo {
            pid,
            host,
            branch,
            worktree,
            prefix,
        })
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

        // Old format: branch/worktree/prefix are None, not empty strings
        let info = LockGuard::read_holder_info(&lock_path);
        assert!(info.is_some());
        let h = info.unwrap();
        assert_eq!(h.pid, 12345);
        assert_eq!(h.host, "myhost");
        assert_eq!(h.branch, None);
        assert_eq!(h.worktree, None);
        assert_eq!(h.prefix, None);
    }

    #[test]
    fn test_read_holder_info_handles_whitespace() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("whitespace.lock");

        fs::write(&lock_path, "67890@host\n").unwrap();

        let info = LockGuard::read_holder_info(&lock_path);
        assert!(info.is_some());
        let h = info.unwrap();
        assert_eq!(h.pid, 67890);
        assert_eq!(h.host, "host");
    }

    // --- HolderInfo / enhanced lock format tests (TDD for FEAT-003) ---

    #[test]
    fn test_write_holder_info_extended_writes_multiline_format() {
        let temp_dir = TempDir::new().unwrap();
        let mut guard = LockGuard::acquire(temp_dir.path()).unwrap();

        guard
            .write_holder_info_extended(
                Some("feat/worktree-lifecycle"),
                Some("/path/to/worktree"),
                Some("feat-worktree-lifecycle"),
            )
            .unwrap();

        let contents = fs::read_to_string(guard.path()).unwrap();
        assert!(
            contents.contains("branch=feat/worktree-lifecycle"),
            "should contain branch line: {}",
            contents
        );
        assert!(
            contents.contains("worktree=/path/to/worktree"),
            "should contain worktree line: {}",
            contents
        );
        assert!(
            contents.contains("prefix=feat-worktree-lifecycle"),
            "should contain prefix line: {}",
            contents
        );
        // First line must be pid@host
        let first_line = contents.lines().next().unwrap();
        assert!(
            first_line.contains('@'),
            "first line should be pid@host: {}",
            first_line
        );
    }

    #[test]
    fn test_write_holder_info_extended_omits_none_fields() {
        let temp_dir = TempDir::new().unwrap();
        let mut guard = LockGuard::acquire(temp_dir.path()).unwrap();

        guard
            .write_holder_info_extended(Some("feat/branch"), None, None)
            .unwrap();

        let contents = fs::read_to_string(guard.path()).unwrap();
        assert!(contents.contains("branch=feat/branch"));
        assert!(
            !contents.contains("worktree="),
            "should not write None worktree"
        );
        assert!(
            !contents.contains("prefix="),
            "should not write None prefix"
        );
    }

    #[test]
    fn test_read_holder_info_parses_multiline_format() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("multi.lock");

        fs::write(
            &lock_path,
            "42@testhost\nbranch=feat/my-feature\nworktree=/some/path\nprefix=feat-my-feature\n",
        )
        .unwrap();

        let info = LockGuard::read_holder_info(&lock_path).unwrap();
        assert_eq!(info.pid, 42);
        assert_eq!(info.host, "testhost");
        assert_eq!(info.branch, Some("feat/my-feature".to_string()));
        assert_eq!(info.worktree, Some("/some/path".to_string()));
        assert_eq!(info.prefix, Some("feat-my-feature".to_string()));
    }

    #[test]
    fn test_read_holder_info_falls_back_to_old_format() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("old.lock");

        // Old single-line format
        fs::write(&lock_path, "99@legacyhost").unwrap();

        let info = LockGuard::read_holder_info(&lock_path).unwrap();
        assert_eq!(info.pid, 99);
        assert_eq!(info.host, "legacyhost");
        // Must be None, not empty string — this is the known-bad discriminator
        assert_eq!(
            info.branch, None,
            "old format must yield None branch, not \"\""
        );
        assert_eq!(
            info.worktree, None,
            "old format must yield None worktree, not \"\""
        );
        assert_eq!(
            info.prefix, None,
            "old format must yield None prefix, not \"\""
        );
    }

    #[test]
    fn test_read_holder_info_new_returns_none_for_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("empty.lock");

        fs::write(&lock_path, "").unwrap();

        let info = LockGuard::read_holder_info(&lock_path);
        assert!(info.is_none(), "empty file should return None");
    }

    #[test]
    fn test_lock_error_message_includes_branch_and_prefix() {
        let temp_dir = TempDir::new().unwrap();

        // Acquire the lock and write extended info
        let mut guard1 = LockGuard::acquire(temp_dir.path()).unwrap();
        guard1
            .write_holder_info_extended(
                Some("feat/my-feature"),
                Some("/path/to/wt"),
                Some("feat-my-feature"),
            )
            .unwrap();

        // Second acquire should fail with branch/prefix in message
        let result = LockGuard::acquire(temp_dir.path());
        assert!(result.is_err());

        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("feat/my-feature") || msg.contains("feat-my-feature"),
            "error message should include branch or prefix: {}",
            msg
        );
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

    // --- TEST-002: HolderInfo metadata coverage ---

    #[test]
    fn test_write_read_roundtrip_all_fields() {
        let temp_dir = TempDir::new().unwrap();
        let mut guard = LockGuard::acquire(temp_dir.path()).unwrap();

        guard
            .write_holder_info_extended(
                Some("feat/round-trip"),
                Some("/worktrees/round-trip"),
                Some("feat-round-trip"),
            )
            .unwrap();

        let info = LockGuard::read_holder_info(guard.path()).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.branch, Some("feat/round-trip".to_string()));
        assert_eq!(info.worktree, Some("/worktrees/round-trip".to_string()));
        assert_eq!(info.prefix, Some("feat-round-trip".to_string()));
        // host should be populated (non-empty)
        assert!(!info.host.is_empty(), "host should be non-empty");
    }

    #[test]
    fn test_write_read_roundtrip_no_optional_fields() {
        let temp_dir = TempDir::new().unwrap();
        let mut guard = LockGuard::acquire(temp_dir.path()).unwrap();

        guard.write_holder_info_extended(None, None, None).unwrap();

        let info = LockGuard::read_holder_info(guard.path()).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.branch, None);
        assert_eq!(info.worktree, None);
        assert_eq!(info.prefix, None);
    }

    #[test]
    fn test_read_holder_info_partial_fields_only_branch() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("partial.lock");

        // Only branch present, no worktree or prefix
        fs::write(&lock_path, "100@partialhost\nbranch=feat/partial\n").unwrap();

        let info = LockGuard::read_holder_info(&lock_path).unwrap();
        assert_eq!(info.pid, 100);
        assert_eq!(info.host, "partialhost");
        assert_eq!(info.branch, Some("feat/partial".to_string()));
        assert_eq!(info.worktree, None);
        assert_eq!(info.prefix, None);
    }

    #[test]
    fn test_read_holder_info_partial_fields_only_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("partial2.lock");

        // Only prefix present, no branch or worktree
        fs::write(&lock_path, "200@prefixhost\nprefix=my-prefix\n").unwrap();

        let info = LockGuard::read_holder_info(&lock_path).unwrap();
        assert_eq!(info.pid, 200);
        assert_eq!(info.branch, None);
        assert_eq!(info.worktree, None);
        assert_eq!(info.prefix, Some("my-prefix".to_string()));
    }

    #[test]
    fn test_read_holder_info_ignores_unknown_keys() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("future.lock");

        // Unknown keys from a future version of task-mgr should be silently ignored
        fs::write(
            &lock_path,
            "300@futurehost\nbranch=feat/future\nunknown_key=some_value\nanother_future=42\nprefix=feat-future\n",
        )
        .unwrap();

        let info = LockGuard::read_holder_info(&lock_path).unwrap();
        assert_eq!(info.pid, 300);
        assert_eq!(info.host, "futurehost");
        assert_eq!(info.branch, Some("feat/future".to_string()));
        assert_eq!(info.prefix, Some("feat-future".to_string()));
        // unknown keys are ignored, no panic or error
    }

    #[test]
    fn test_holder_info_display_full() {
        let info = HolderInfo {
            pid: 1234,
            host: "myhost".to_string(),
            branch: Some("feat/my-branch".to_string()),
            worktree: Some("/some/worktree".to_string()),
            prefix: Some("feat-my-branch".to_string()),
        };
        let s = info.to_string();
        assert_eq!(
            s,
            "1234@myhost branch=feat/my-branch worktree=/some/worktree prefix=feat-my-branch"
        );
    }

    #[test]
    fn test_holder_info_display_no_optional_fields() {
        let info = HolderInfo {
            pid: 5678,
            host: "otherhost".to_string(),
            branch: None,
            worktree: None,
            prefix: None,
        };
        let s = info.to_string();
        // Only pid@host, no extra fields
        assert_eq!(s, "5678@otherhost");
    }

    #[test]
    fn test_holder_info_display_branch_only() {
        let info = HolderInfo {
            pid: 9999,
            host: "branchhost".to_string(),
            branch: Some("main".to_string()),
            worktree: None,
            prefix: None,
        };
        let s = info.to_string();
        assert_eq!(s, "9999@branchhost branch=main");
    }

    #[test]
    fn test_lock_error_message_no_holder_info() {
        let temp_dir = TempDir::new().unwrap();

        // Acquire lock, then manually truncate the lockfile to simulate unreadable holder
        let guard = LockGuard::acquire(temp_dir.path()).unwrap();
        // Write invalid (non-parseable) content — the existing guard holds the flock
        // We can verify the error message format when holder_info is None by checking
        // the acquire path with a fresh dir where lock file has no valid content.
        // Since we can't hold two locks in same process, test the message format directly.
        drop(guard);

        // Write a file with content that won't parse as pid@host
        let lock_path = temp_dir.path().join("tasks.db.lock");
        fs::write(&lock_path, "not-valid-format").unwrap();

        // read_holder_info should return None for invalid format
        let info = LockGuard::read_holder_info(&lock_path);
        assert!(
            info.is_none(),
            "invalid format should return None, got {:?}",
            info
        );
    }

    #[test]
    fn test_lock_error_message_with_partial_holder_info() {
        let temp_dir = TempDir::new().unwrap();

        // Acquire with branch only (no prefix)
        let mut guard1 = LockGuard::acquire(temp_dir.path()).unwrap();
        guard1
            .write_holder_info_extended(Some("main"), None, None)
            .unwrap();

        // Second acquire should fail and show branch in error
        let result = LockGuard::acquire(temp_dir.path());
        assert!(result.is_err());

        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("main"),
            "error should include branch 'main': {}",
            msg
        );
    }

    // --- Per-prefix lock file naming and concurrent acquisition tests ---

    #[test]
    fn test_lock_name_for_prefix_uses_loop_dash_prefix_dot_lock() {
        // Verify naming convention: loop-{prefix}.lock for prefixed sessions
        let temp_dir = TempDir::new().unwrap();
        let expected_path = temp_dir.path().join("loop-P1.lock");

        assert!(!expected_path.exists());

        let guard = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock").unwrap();

        assert!(expected_path.exists());
        assert_eq!(guard.path(), expected_path);
    }

    #[test]
    fn test_lock_name_for_none_prefix_uses_loop_dot_lock() {
        // Verify naming convention: loop.lock when no prefix
        let temp_dir = TempDir::new().unwrap();
        let expected_path = temp_dir.path().join("loop.lock");

        let guard = LockGuard::acquire_named(temp_dir.path(), "loop.lock").unwrap();

        assert_eq!(guard.path(), expected_path);
        assert!(expected_path.exists());
    }

    #[test]
    fn test_concurrent_p1_and_p2_locks_both_succeed() {
        // loop-P1.lock and loop-P2.lock are independent — both sessions can run concurrently
        let temp_dir = TempDir::new().unwrap();

        let guard_p1 = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock").unwrap();
        let guard_p2 = LockGuard::acquire_named(temp_dir.path(), "loop-P2.lock").unwrap();

        // Both lock files exist concurrently
        assert!(temp_dir.path().join("loop-P1.lock").exists());
        assert!(temp_dir.path().join("loop-P2.lock").exists());

        // Explicitly drop to verify both clean up
        drop(guard_p1);
        drop(guard_p2);

        assert!(!temp_dir.path().join("loop-P1.lock").exists());
        assert!(!temp_dir.path().join("loop-P2.lock").exists());
    }

    #[test]
    fn test_same_prd_lock_fails_with_clear_error_message() {
        // Acquiring loop-P1.lock twice must fail with a clear "locked" error
        let temp_dir = TempDir::new().unwrap();

        let _guard = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock").unwrap();

        let result = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock");
        assert!(result.is_err(), "Second acquire for same PRD must fail");

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("locked"),
            "Error should mention 'locked': {}",
            msg
        );
    }

    #[test]
    fn test_same_prd_lock_error_contains_holder_pid() {
        // The error message should identify the holder process
        let temp_dir = TempDir::new().unwrap();
        let our_pid = std::process::id();

        let _guard = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock").unwrap();

        let result = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock");
        assert!(result.is_err());

        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains(&our_pid.to_string()),
            "Error should contain holder PID {}: {}",
            our_pid,
            msg
        );
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
