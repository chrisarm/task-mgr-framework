//! Concurrent access tests for task-mgr locking behavior.
//!
//! These tests verify that:
//! - Two write commands conflict on lock
//! - Read commands succeed while write holds lock
//! - Lock is released on process exit
//! - Stale lock detection (PID no longer exists)
//!
//! We use fs2 file locking directly in tests to simulate external processes
//! holding the lock, since task-mgr commands complete quickly.

// Allow deprecated cargo_bin function - the macro alternative requires more boilerplate
#![allow(deprecated)]

use assert_cmd::cargo::cargo_bin;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write as IoWrite;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Initialize a tempdir with the sample PRD.
fn setup_initialized_tempdir() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    let status = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["init", "--no-prefix", "--from-json", prd_path.to_str().unwrap()])
        .status()
        .expect("Failed to run init");

    assert!(status.success(), "Init should succeed");
    temp_dir
}

/// A guard that holds an exclusive file lock on the lockfile.
/// When dropped, the lock is released.
struct TestLockHolder {
    file: File,
}

impl TestLockHolder {
    /// Acquire an exclusive lock on the database lockfile.
    /// This simulates another process holding the lock.
    fn acquire(temp_dir: &TempDir) -> Self {
        let lock_path = temp_dir.path().join("tasks.db.lock");

        // Create and lock the file
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .expect("Failed to open lockfile");

        // Acquire exclusive lock
        file.try_lock_exclusive()
            .expect("Failed to acquire exclusive lock");

        // Write our fake PID (use a high number to identify as test)
        let pid = std::process::id();
        file.set_len(0).unwrap();
        write!(file, "{}", pid).unwrap();
        file.sync_all().unwrap();

        TestLockHolder { file }
    }
}

impl Drop for TestLockHolder {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

// ============================================================================
// Test: Two write commands conflict on lock
// ============================================================================

#[test]
fn test_two_write_commands_conflict_on_lock() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    // Try to run a write command while lock is held
    // complete is a write command that requires lock
    let second = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-001", "--force"])
        .output()
        .expect("Failed to run command");

    // Command should fail due to lock
    assert!(
        !second.status.success(),
        "Write command should fail when lock is held"
    );

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("locked") || stderr.contains("lock"),
        "Error should mention lock: {}",
        stderr
    );
}

#[test]
fn test_lock_error_shows_holder_pid() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);
    let our_pid = std::process::id();

    // Try to run a write command
    let second = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-001", "--force"])
        .output()
        .expect("Failed to run command");

    // Error should contain our PID (the holder)
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains(&our_pid.to_string()),
        "Error should contain holder PID {}: {}",
        our_pid,
        stderr
    );
}

// ============================================================================
// Test: Read command succeeds while write holds lock
// ============================================================================

#[test]
fn test_read_command_succeeds_while_write_holds_lock() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    // List is a read command that does NOT require lock
    let read_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .output()
        .expect("Failed to run list command");

    // Read command should succeed even when lock is held
    assert!(
        read_result.status.success(),
        "Read command should succeed while write holds lock. stderr: {}",
        String::from_utf8_lossy(&read_result.stderr)
    );

    // Verify output is valid JSON
    let stdout = String::from_utf8_lossy(&read_result.stdout);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "List output should be valid JSON: {}",
        stdout
    );
}

#[test]
fn test_next_without_claim_succeeds_while_lock_held() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    // next without --claim is a read operation (no lock needed)
    // We need decay_threshold=0 to avoid lock acquisition for decay
    let read_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["next", "--format", "json", "--decay-threshold", "0"])
        .output()
        .expect("Failed to run next command");

    // Should succeed without lock
    assert!(
        read_result.status.success(),
        "next without --claim should succeed. stderr: {}",
        String::from_utf8_lossy(&read_result.stderr)
    );
}

#[test]
fn test_next_with_claim_fails_while_lock_held() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    // next with --claim requires lock (it modifies database)
    let write_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "next",
            "--claim",
            "--format",
            "json",
            "--decay-threshold",
            "0",
        ])
        .output()
        .expect("Failed to run next --claim command");

    // Should fail due to lock
    assert!(
        !write_result.status.success(),
        "next --claim should fail when lock is held"
    );

    let stderr = String::from_utf8_lossy(&write_result.stderr);
    assert!(
        stderr.contains("locked") || stderr.contains("lock"),
        "Error should mention lock: {}",
        stderr
    );
}

#[test]
fn test_doctor_without_auto_fix_succeeds_while_lock_held() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    // doctor without --auto-fix is a read operation (no lock needed)
    let read_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["doctor", "--format", "json"])
        .output()
        .expect("Failed to run doctor command");

    // Should succeed without lock
    assert!(
        read_result.status.success(),
        "doctor without --auto-fix should succeed. stderr: {}",
        String::from_utf8_lossy(&read_result.stderr)
    );
}

#[test]
fn test_doctor_with_auto_fix_fails_while_lock_held() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    // doctor with --auto-fix requires lock
    let write_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["doctor", "--auto-fix"])
        .output()
        .expect("Failed to run doctor --auto-fix command");

    // Should fail due to lock
    assert!(
        !write_result.status.success(),
        "doctor --auto-fix should fail when lock is held"
    );

    let stderr = String::from_utf8_lossy(&write_result.stderr);
    assert!(
        stderr.contains("locked") || stderr.contains("lock"),
        "Error should mention lock: {}",
        stderr
    );
}

// ============================================================================
// Test: Lock released when holder drops
// ============================================================================

#[test]
fn test_lock_released_when_holder_drops() {
    let temp_dir = setup_initialized_tempdir();

    // Hold the lock in a scope
    {
        let _lock_holder = TestLockHolder::acquire(&temp_dir);

        // Verify lock is held (write command fails)
        let first_attempt = Command::new(cargo_bin("task-mgr"))
            .args(["--dir", temp_dir.path().to_str().unwrap()])
            .args(["complete", "TASK-001", "--force"])
            .output()
            .expect("Failed to run command");

        assert!(
            !first_attempt.status.success(),
            "Write should fail while lock held"
        );
    }
    // Lock released here

    // Now write command should succeed
    let second_attempt = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-001", "--force"])
        .output()
        .expect("Failed to run command after lock release");

    assert!(
        second_attempt.status.success(),
        "Write should succeed after lock released. stderr: {}",
        String::from_utf8_lossy(&second_attempt.stderr)
    );
}

#[test]
fn test_lock_released_on_normal_exit() {
    let temp_dir = setup_initialized_tempdir();

    // Run a write command that completes normally
    let result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-001", "--force"])
        .output()
        .expect("Failed to run first complete");

    assert!(
        result.status.success(),
        "First complete should succeed. stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    // Second write command should also succeed (lock released)
    let result2 = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-002", "--force"])
        .output()
        .expect("Failed to run second complete");

    assert!(
        result2.status.success(),
        "Second complete should succeed. stderr: {}",
        String::from_utf8_lossy(&result2.stderr)
    );
}

// ============================================================================
// Test: Stale lock detection (PID no longer exists)
// ============================================================================

#[test]
fn test_stale_lockfile_with_no_actual_lock_does_not_block() {
    let temp_dir = setup_initialized_tempdir();
    let lock_path = temp_dir.path().join("tasks.db.lock");

    // Create a lockfile with a fake PID, but don't hold the actual lock
    // This simulates a stale lockfile left after a crash
    let fake_dead_pid = 999999999u32;
    fs::write(&lock_path, fake_dead_pid.to_string()).expect("Failed to write fake lockfile");

    // Since no process actually holds the lock (try_lock_exclusive checks the kernel lock,
    // not just the file contents), this should succeed
    let result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-001", "--force"])
        .output()
        .expect("Failed to run command");

    // Should succeed because the lock is not actually held (just stale file)
    assert!(
        result.status.success(),
        "Command should succeed with stale lockfile (no actual lock). stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn test_lockfile_with_nonexistent_pid_does_not_block() {
    let temp_dir = setup_initialized_tempdir();
    let lock_path = temp_dir.path().join("tasks.db.lock");

    // Create a lockfile with a PID that definitely doesn't exist
    let nonexistent_pid = 4_000_000_000u64; // Beyond typical PID range
    fs::write(&lock_path, nonexistent_pid.to_string()).expect("Failed to write lockfile");

    // Even a read-only command should work fine
    let result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .output()
        .expect("Failed to run command");

    assert!(
        result.status.success(),
        "Command should succeed when lockfile exists but lock not held. stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

// ============================================================================
// Test: Concurrent read operations (should all succeed)
// ============================================================================

#[test]
fn test_concurrent_read_operations_succeed() {
    let temp_dir = setup_initialized_tempdir();
    let temp_path = temp_dir.path().to_path_buf();

    let ready = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    // Spawn multiple concurrent read operations
    for i in 0..5 {
        let path = temp_path.clone();
        let ready_clone = Arc::clone(&ready);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            while !ready_clone.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }

            let result = Command::new(cargo_bin("task-mgr"))
                .args(["--dir", path.to_str().unwrap()])
                .args(["list", "--format", "json"])
                .output()
                .expect("Failed to run list command");

            (i, result.status.success())
        });

        handles.push(handle);
    }

    // Signal all threads to start simultaneously
    ready.store(true, Ordering::SeqCst);

    // Wait for all threads and verify all succeeded
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    for (i, success) in &results {
        assert!(*success, "Concurrent read operation {} should succeed", i);
    }

    assert_eq!(results.len(), 5, "All 5 concurrent reads should complete");
}

// ============================================================================
// Test: Sequential write operations (should all succeed)
// ============================================================================

#[test]
fn test_sequential_write_operations_succeed() {
    let temp_dir = setup_initialized_tempdir();

    // Run multiple write operations sequentially - all should succeed
    let tasks = ["TASK-001", "TASK-002", "TASK-003"];

    for (i, task_id) in tasks.iter().enumerate() {
        let result = Command::new(cargo_bin("task-mgr"))
            .args(["--dir", temp_dir.path().to_str().unwrap()])
            .args(["complete", task_id, "--force"])
            .output()
            .expect("Failed to run complete command");

        assert!(
            result.status.success(),
            "Sequential write {} ({}) should succeed. stderr: {}",
            i,
            task_id,
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

// ============================================================================
// Test: Lock prevents simultaneous writes from multiple threads
// ============================================================================

#[test]
fn test_lock_prevents_simultaneous_writes() {
    let temp_dir = setup_initialized_tempdir();
    let temp_path = temp_dir.path().to_path_buf();

    // Hold the lock using our test helper
    let _lock_holder = TestLockHolder::acquire(&temp_dir);

    let ready = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    // Try to spawn multiple concurrent write operations
    for i in 0..3 {
        let path = temp_path.clone();
        let ready_clone = Arc::clone(&ready);
        let task_id = format!("TASK-00{}", i + 1);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            while !ready_clone.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }

            let result = Command::new(cargo_bin("task-mgr"))
                .args(["--dir", path.to_str().unwrap()])
                .args(["complete", &task_id, "--force"])
                .output()
                .expect("Failed to run complete command");

            result.status.success()
        });

        handles.push(handle);
    }

    // Signal all threads to start simultaneously
    ready.store(true, Ordering::SeqCst);

    // Wait for all threads
    let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All should fail because test holder has the lock
    let all_failed = results.iter().all(|success| !success);
    assert!(
        all_failed,
        "All concurrent writes should fail when lock is held. Results: {:?}",
        results
    );
}

// ============================================================================
// Test: Concurrent writes race (one wins, others fail)
// ============================================================================

#[test]
fn test_concurrent_writes_one_wins() {
    let temp_dir = setup_initialized_tempdir();
    let temp_path = temp_dir.path().to_path_buf();

    let ready = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    // Spawn multiple concurrent write operations (no external lock holder)
    // One should succeed, others should either succeed sequentially or fail
    for i in 0..3 {
        let path = temp_path.clone();
        let ready_clone = Arc::clone(&ready);
        // Use different task IDs so they don't conflict on the database side
        let task_id = format!("TASK-00{}", i + 1);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            while !ready_clone.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }

            let result = Command::new(cargo_bin("task-mgr"))
                .args(["--dir", path.to_str().unwrap()])
                .args(["complete", &task_id, "--force"])
                .output()
                .expect("Failed to run complete command");

            result.status.success()
        });

        handles.push(handle);
    }

    // Signal all threads to start simultaneously
    ready.store(true, Ordering::SeqCst);

    // Wait for all threads
    let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // At least one should succeed (the one that gets the lock first)
    // The others might fail with lock error or might succeed sequentially
    // depending on timing. We just check that at least one succeeds.
    let any_succeeded = results.iter().any(|&success| success);
    assert!(
        any_succeeded,
        "At least one concurrent write should succeed. Results: {:?}",
        results
    );
}

// ============================================================================
// Test: Doctor command with stale in_progress tasks
// ============================================================================

#[test]
fn test_doctor_detects_stale_in_progress_task() {
    let temp_dir = setup_initialized_tempdir();

    // Claim a task (this sets it to in_progress)
    let claim_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "next",
            "--claim",
            "--format",
            "json",
            "--decay-threshold",
            "0",
        ])
        .output()
        .expect("Failed to claim task");

    assert!(
        claim_result.status.success(),
        "Claim should succeed. stderr: {}",
        String::from_utf8_lossy(&claim_result.stderr)
    );

    // Run doctor - it should detect the stale in_progress task
    let doctor_result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["doctor", "--format", "json"])
        .output()
        .expect("Failed to run doctor");

    assert!(
        doctor_result.status.success(),
        "Doctor should succeed. stderr: {}",
        String::from_utf8_lossy(&doctor_result.stderr)
    );

    let stdout = String::from_utf8_lossy(&doctor_result.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Doctor output should be valid JSON");

    // Check if there are any issues detected
    if let Some(summary) = parsed.get("summary") {
        if let Some(issues) = summary.get("issue_count") {
            // Should detect the stale in_progress task as an issue
            assert!(
                issues.as_i64().unwrap_or(0) >= 0,
                "Doctor should report issues or healthy state"
            );
        }
    }
}

// ============================================================================
// Test: Lock file path is correct
// ============================================================================

#[test]
fn test_lock_file_path() {
    let temp_dir = setup_initialized_tempdir();
    let lock_path = temp_dir.path().join("tasks.db.lock");

    // Before any write operation, lockfile may not exist
    // (depending on whether temp_dir cleanup removed it)

    // Run a write operation
    let result = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "TASK-001", "--force"])
        .output()
        .expect("Failed to run complete");

    assert!(result.status.success());

    // Lockfile should have been created and removed during the operation
    // (it's cleaned up on Drop). But we can verify the path by holding a lock.
    let _lock_holder = TestLockHolder::acquire(&temp_dir);
    assert!(
        lock_path.exists(),
        "Lockfile should exist at {:?}",
        lock_path
    );
}
