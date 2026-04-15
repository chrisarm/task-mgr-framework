//! Deadline tracking for the autonomous agent loop.
//!
//! Manages time-based deadlines via `.deadline-<basename>` files that store
//! epoch seconds. The loop checks these at iteration boundaries to enforce
//! time budgets.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::TaskMgrResult;

use super::DEADLINE_FILE_PREFIX;

/// Maximum allowed deadline in hours (1 week).
const MAX_DEADLINE_HOURS: f64 = 168.0;

/// Create a deadline file for the given PRD basename.
///
/// Writes the deadline epoch (current time + hours) to `.deadline-<basename>`.
///
/// # Errors
///
/// Returns error if hours is not positive, exceeds MAX_DEADLINE_HOURS,
/// or if the file cannot be written.
pub fn create_deadline(tasks_dir: &Path, prd_basename: &str, hours: f64) -> TaskMgrResult<PathBuf> {
    if hours <= 0.0 {
        return Err(crate::TaskMgrError::IoErrorWithContext {
            file_path: deadline_path(tasks_dir, prd_basename).display().to_string(),
            operation: "creating deadline".to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("deadline hours must be positive, got {}", hours),
            ),
        });
    }

    if hours > MAX_DEADLINE_HOURS {
        return Err(crate::TaskMgrError::IoErrorWithContext {
            file_path: deadline_path(tasks_dir, prd_basename).display().to_string(),
            operation: "creating deadline".to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "deadline hours {} exceeds maximum {}",
                    hours, MAX_DEADLINE_HOURS
                ),
            ),
        });
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs();

    let deadline_secs = now + (hours * 3600.0) as u64;
    let path = deadline_path(tasks_dir, prd_basename);

    fs::write(&path, deadline_secs.to_string()).map_err(|e| {
        crate::TaskMgrError::IoErrorWithContext {
            file_path: path.display().to_string(),
            operation: "writing deadline file".to_string(),
            source: e,
        }
    })?;

    Ok(path)
}

/// Check if the deadline has passed for the given PRD basename.
///
/// Returns `true` if the deadline file exists and the current time exceeds
/// the stored epoch. Returns `false` if no deadline file exists or the
/// file content is invalid.
pub fn check_deadline(tasks_dir: &Path, prd_basename: &str) -> bool {
    let path = deadline_path(tasks_dir, prd_basename);

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return false, // No deadline file
    };

    let deadline_epoch: u64 = match content.trim().parse() {
        Ok(e) => e,
        Err(_) => {
            eprintln!(
                "Warning: invalid deadline file content in {}: '{}'",
                path.display(),
                content.trim()
            );
            return false;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs();

    now >= deadline_epoch
}

/// Remove the deadline file for the given PRD basename.
///
/// No-op if the file doesn't exist.
pub fn cleanup_deadline(tasks_dir: &Path, prd_basename: &str) {
    let path = deadline_path(tasks_dir, prd_basename);
    if path.exists()
        && let Err(e) = fs::remove_file(&path) {
            eprintln!(
                "Warning: could not remove deadline file {}: {}",
                path.display(),
                e
            );
        }
}

/// Get the deadline file path for a PRD basename.
fn deadline_path(tasks_dir: &Path, prd_basename: &str) -> PathBuf {
    tasks_dir.join(format!("{}{}", DEADLINE_FILE_PREFIX, prd_basename))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // --- create_deadline tests ---

    #[test]
    fn test_create_deadline_writes_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_deadline(temp_dir.path(), "test-prd", 1.0);
        assert!(result.is_ok());

        let path = result.unwrap();
        assert!(path.exists());

        let content = fs::read_to_string(&path).unwrap();
        let epoch: u64 = content.trim().parse().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Deadline should be approximately now + 3600 (within 5 seconds)
        assert!(epoch > now + 3590);
        assert!(epoch < now + 3610);
    }

    #[test]
    fn test_create_deadline_fractional_hours() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_deadline(temp_dir.path(), "test-prd", 0.5);
        assert!(result.is_ok());

        let path = result.unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let epoch: u64 = content.trim().parse().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 0.5 hours = 1800 seconds
        assert!(epoch > now + 1790);
        assert!(epoch < now + 1810);
    }

    #[test]
    fn test_create_deadline_rejects_zero_hours() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_deadline(temp_dir.path(), "test-prd", 0.0);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_deadline_rejects_negative_hours() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_deadline(temp_dir.path(), "test-prd", -1.0);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_deadline_rejects_excessive_hours() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_deadline(temp_dir.path(), "test-prd", 200.0);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_deadline_max_hours_accepted() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_deadline(temp_dir.path(), "test-prd", 168.0);
        assert!(result.is_ok());
    }

    // --- check_deadline tests ---

    #[test]
    fn test_check_deadline_no_file_returns_false() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!check_deadline(temp_dir.path(), "test-prd"));
    }

    #[test]
    fn test_check_deadline_future_returns_false() {
        let temp_dir = TempDir::new().unwrap();
        let future_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        fs::write(
            temp_dir.path().join(".deadline-test-prd"),
            future_epoch.to_string(),
        )
        .unwrap();

        assert!(!check_deadline(temp_dir.path(), "test-prd"));
    }

    #[test]
    fn test_check_deadline_past_returns_true() {
        let temp_dir = TempDir::new().unwrap();
        let past_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 10; // 10 seconds ago
        fs::write(
            temp_dir.path().join(".deadline-test-prd"),
            past_epoch.to_string(),
        )
        .unwrap();

        assert!(check_deadline(temp_dir.path(), "test-prd"));
    }

    #[test]
    fn test_check_deadline_invalid_content_returns_false() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".deadline-test-prd"), "not-a-number").unwrap();

        assert!(!check_deadline(temp_dir.path(), "test-prd"));
    }

    // --- cleanup_deadline tests ---

    #[test]
    fn test_cleanup_deadline_removes_file() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join(".deadline-test-prd");
        fs::write(&path, "12345").unwrap();
        assert!(path.exists());

        cleanup_deadline(temp_dir.path(), "test-prd");
        assert!(!path.exists());
    }

    #[test]
    fn test_cleanup_deadline_nonexistent_is_noop() {
        let temp_dir = TempDir::new().unwrap();
        cleanup_deadline(temp_dir.path(), "test-prd");
        // Should not panic
    }

    // --- deadline_path tests ---

    #[test]
    fn test_deadline_path_format() {
        let path = deadline_path(Path::new("/tasks"), "my-prd");
        assert_eq!(path, PathBuf::from("/tasks/.deadline-my-prd"));
    }
}
