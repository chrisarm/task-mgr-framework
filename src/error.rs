//! Error types for task-mgr CLI tool.
//!
//! Provides structured error handling with variants for database operations,
//! I/O, JSON parsing, locking, and domain-specific errors.
//!
//! # User-friendly error messages
//!
//! All error variants are designed to provide actionable information:
//! - `DatabaseError` includes file path and operation context when available
//! - `LockError` includes hints on how to resolve stuck locks
//! - `NotFound` includes the resource type and identifier
//! - `InvalidState` includes both expected and actual states

use thiserror::Error;

/// Result type alias for task-mgr operations.
pub type TaskMgrResult<T> = Result<T, TaskMgrError>;

/// Error types for task-mgr operations.
#[derive(Debug, Error)]
pub enum TaskMgrError {
    /// Database operation failed (simple wrapper).
    #[error("Database error: {0}")]
    DatabaseError(#[from] rusqlite::Error),

    /// Database operation failed with context.
    #[error("Database error during {operation} on '{file_path}': {source}")]
    DatabaseErrorWithContext {
        /// The database file path
        file_path: String,
        /// The operation being performed (e.g., "opening connection", "executing query")
        operation: String,
        /// The underlying database error
        #[source]
        source: rusqlite::Error,
    },

    /// I/O operation failed.
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// I/O operation failed with context.
    #[error("I/O error during {operation} on '{file_path}': {source}")]
    IoErrorWithContext {
        /// The file path involved
        file_path: String,
        /// The operation being performed (e.g., "reading", "writing")
        operation: String,
        /// The underlying I/O error
        #[source]
        source: std::io::Error,
    },

    /// JSON serialization/deserialization failed.
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Failed to acquire exclusive lock on database.
    #[error("Lock error: {message}\n\nHint: {hint}")]
    LockError {
        /// Description of the lock failure
        message: String,
        /// Actionable hint for resolving the lock issue
        hint: String,
    },

    /// Requested resource was not found.
    #[error("{resource_type} not found: {id}")]
    NotFound {
        /// Type of resource (e.g., "Task", "Run", "Learning")
        resource_type: String,
        /// Identifier that was not found
        id: String,
    },

    /// Operation attempted on resource in invalid state.
    #[error("Invalid state for {resource_type} '{id}': expected {expected}, found {actual}")]
    InvalidState {
        /// Type of resource
        resource_type: String,
        /// Resource identifier
        id: String,
        /// Expected state(s)
        expected: String,
        /// Actual state found
        actual: String,
    },

    /// Task cannot be completed because its dependencies are not satisfied.
    #[error("Cannot complete task '{task_id}': unsatisfied dependencies: {unsatisfied}\n\n{hint}")]
    DependencyNotSatisfied {
        /// Task identifier
        task_id: String,
        /// Comma-separated list of unsatisfied dependency IDs
        unsatisfied: String,
        /// Actionable hint for resolving the issue
        hint: String,
    },

    /// Invalid status transition attempted.
    #[error(
        "Invalid transition for task '{task_id}': cannot go from '{from}' to '{to}'.\n\n{hint}"
    )]
    InvalidTransition {
        /// Task identifier
        task_id: String,
        /// Current status
        from: String,
        /// Attempted target status
        to: String,
        /// Actionable hint for resolving the issue
        hint: String,
    },

    /// Unsafe path detected (potential path traversal attack).
    #[error("Unsafe path in {context}: '{path}' - {reason}")]
    UnsafePath {
        /// Context where the path was found (e.g., "touchesFiles", "PRD import")
        context: String,
        /// The offending path
        path: String,
        /// Explanation of why the path is unsafe
        reason: String,
    },
}

/// Default hint for lock errors when a specific PID is known.
const LOCK_HINT_WITH_PID: &str = "Wait for the other process to finish, or if it's stuck, \
terminate process with 'kill <PID>' and remove the lockfile.";

/// Default hint for lock errors when no PID is known.
const LOCK_HINT_NO_PID: &str = "Wait for the other process to finish, or if it's stuck, \
check for running task-mgr processes and remove the lockfile at .task-mgr/tasks.db.lock.";

/// Default hint for general lock acquisition failures.
const LOCK_HINT_GENERAL: &str =
    "Check file permissions and ensure the .task-mgr directory is writable.";

impl TaskMgrError {
    /// Creates a LockError with the given message and a default hint.
    #[must_use]
    pub fn lock_error(message: impl Into<String>) -> Self {
        let msg = message.into();
        let hint = if msg.contains("PID:") {
            LOCK_HINT_WITH_PID
        } else if msg.contains("locked by another process") {
            LOCK_HINT_NO_PID
        } else {
            LOCK_HINT_GENERAL
        }
        .to_string();

        TaskMgrError::LockError { message: msg, hint }
    }

    /// Creates a LockError with a custom hint.
    #[must_use]
    pub fn lock_error_with_hint(message: impl Into<String>, hint: impl Into<String>) -> Self {
        TaskMgrError::LockError {
            message: message.into(),
            hint: hint.into(),
        }
    }

    /// Creates a DatabaseError with file path and operation context.
    #[must_use]
    pub fn database_error(
        file_path: impl Into<String>,
        operation: impl Into<String>,
        source: rusqlite::Error,
    ) -> Self {
        TaskMgrError::DatabaseErrorWithContext {
            file_path: file_path.into(),
            operation: operation.into(),
            source,
        }
    }

    /// Creates an IoError with file path and operation context.
    #[must_use]
    pub fn io_error(
        file_path: impl Into<String>,
        operation: impl Into<String>,
        source: std::io::Error,
    ) -> Self {
        TaskMgrError::IoErrorWithContext {
            file_path: file_path.into(),
            operation: operation.into(),
            source,
        }
    }

    /// Creates a NotFound error for a task.
    #[must_use]
    pub fn task_not_found(id: impl Into<String>) -> Self {
        TaskMgrError::NotFound {
            resource_type: "Task".to_string(),
            id: id.into(),
        }
    }

    /// Creates a NotFound error for a run.
    #[must_use]
    pub fn run_not_found(id: impl Into<String>) -> Self {
        TaskMgrError::NotFound {
            resource_type: "Run".to_string(),
            id: id.into(),
        }
    }

    /// Creates a NotFound error for a learning.
    #[must_use]
    pub fn learning_not_found(id: impl Into<String>) -> Self {
        TaskMgrError::NotFound {
            resource_type: "Learning".to_string(),
            id: id.into(),
        }
    }

    /// Creates an InvalidState error.
    #[must_use]
    pub fn invalid_state(
        resource_type: impl Into<String>,
        id: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        TaskMgrError::InvalidState {
            resource_type: resource_type.into(),
            id: id.into(),
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    /// Creates an InvalidTransition error for task status changes.
    #[must_use]
    pub fn invalid_transition(
        task_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        TaskMgrError::InvalidTransition {
            task_id: task_id.into(),
            from: from.into(),
            to: to.into(),
            hint: hint.into(),
        }
    }

    /// Creates a DependencyNotSatisfied error for task completion gating.
    #[must_use]
    pub fn dependency_not_satisfied(
        task_id: impl Into<String>,
        unsatisfied_ids: Vec<String>,
    ) -> Self {
        let task_id = task_id.into();
        let unsatisfied = unsatisfied_ids.join(", ");
        let hint = format!(
            "Complete these dependencies first, or use --force to override: {}",
            unsatisfied
        );
        TaskMgrError::DependencyNotSatisfied {
            task_id,
            unsatisfied,
            hint,
        }
    }

    /// Creates an UnsafePath error for path traversal attempts.
    #[must_use]
    pub fn unsafe_path(
        context: impl Into<String>,
        path: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        TaskMgrError::UnsafePath {
            context: context.into(),
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// Validates that a file path is safe for use (no path traversal).
///
/// A path is considered safe if:
/// - It does not start with `/` (absolute path on Unix)
/// - It does not start with a drive letter like `C:` (absolute path on Windows)
/// - It does not contain `..` path components
/// - It does not start with `~` (home directory expansion)
///
/// Note: This validation is for paths from untrusted input (PRD files).
/// CLI arguments for --from-json and --to-json are trusted user input.
///
/// # Arguments
///
/// * `path` - The path to validate
/// * `context` - Context for error message (e.g., "touchesFiles")
/// * `task_id` - Optional task ID for better error messages
///
/// # Errors
///
/// Returns `UnsafePath` error if the path is unsafe.
pub fn validate_safe_path(path: &str, context: &str, task_id: Option<&str>) -> TaskMgrResult<()> {
    let context_with_task = if let Some(id) = task_id {
        format!("{} for task '{}'", context, id)
    } else {
        context.to_string()
    };

    // Check for absolute paths (Unix)
    if path.starts_with('/') {
        return Err(TaskMgrError::unsafe_path(
            context_with_task,
            path,
            "absolute paths are not allowed",
        ));
    }

    // Check for absolute paths (Windows)
    if path.len() >= 2 && path.chars().nth(1) == Some(':') {
        let first_char = path.chars().next().unwrap();
        if first_char.is_ascii_alphabetic() {
            return Err(TaskMgrError::unsafe_path(
                context_with_task,
                path,
                "absolute paths are not allowed",
            ));
        }
    }

    // Check for UNC paths (Windows network paths)
    if path.starts_with("\\\\") || path.starts_with("//") {
        return Err(TaskMgrError::unsafe_path(
            context_with_task,
            path,
            "network paths are not allowed",
        ));
    }

    // Check for home directory expansion
    if path.starts_with('~') {
        return Err(TaskMgrError::unsafe_path(
            context_with_task,
            path,
            "home directory paths are not allowed",
        ));
    }

    // Check for parent directory traversal (..)
    // Split by both / and \ to handle cross-platform paths
    for component in path.split(['/', '\\']) {
        if component == ".." {
            return Err(TaskMgrError::unsafe_path(
                context_with_task,
                path,
                "parent directory traversal (..) is not allowed",
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_not_found_error() {
        let err = TaskMgrError::task_not_found("US-001");
        assert_eq!(err.to_string(), "Task not found: US-001");
    }

    #[test]
    fn test_run_not_found_error() {
        let err = TaskMgrError::run_not_found("abc-123");
        assert_eq!(err.to_string(), "Run not found: abc-123");
    }

    #[test]
    fn test_learning_not_found_error() {
        let err = TaskMgrError::learning_not_found("42");
        assert_eq!(err.to_string(), "Learning not found: 42");
    }

    #[test]
    fn test_lock_error_with_pid_includes_hint() {
        let err = TaskMgrError::lock_error("Database is locked by another process (PID: 12345)");
        let msg = err.to_string();
        // Should contain the message and a hint
        assert!(msg.contains("Database is locked by another process (PID: 12345)"));
        assert!(msg.contains("Hint:"));
        assert!(msg.contains("kill"));
    }

    #[test]
    fn test_lock_error_without_pid_includes_hint() {
        let err = TaskMgrError::lock_error("Database is locked by another process");
        let msg = err.to_string();
        assert!(msg.contains("Database is locked by another process"));
        assert!(msg.contains("Hint:"));
        assert!(msg.contains("lockfile"));
    }

    #[test]
    fn test_lock_error_general_includes_hint() {
        let err = TaskMgrError::lock_error("Failed to acquire lock: permission denied");
        let msg = err.to_string();
        assert!(msg.contains("Failed to acquire lock"));
        assert!(msg.contains("Hint:"));
        assert!(msg.contains("permissions"));
    }

    #[test]
    fn test_lock_error_with_custom_hint() {
        let err =
            TaskMgrError::lock_error_with_hint("Custom lock message", "Custom hint for the user");
        let msg = err.to_string();
        assert!(msg.contains("Custom lock message"));
        assert!(msg.contains("Custom hint for the user"));
    }

    #[test]
    fn test_invalid_state_error() {
        let err = TaskMgrError::invalid_state("Task", "US-001", "todo or in_progress", "done");
        assert_eq!(
            err.to_string(),
            "Invalid state for Task 'US-001': expected todo or in_progress, found done"
        );
    }

    #[test]
    fn test_database_error_from() {
        // Create a rusqlite error by attempting to access a non-existent column
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE test (id INTEGER)", []).unwrap();
        let mut stmt = conn.prepare("SELECT id FROM test").unwrap();
        let result: Result<String, rusqlite::Error> = stmt.query_row([], |row| row.get(999));

        if let Err(rusqlite_err) = result {
            let err: TaskMgrError = rusqlite_err.into();
            assert!(err.to_string().starts_with("Database error:"));
        }
    }

    #[test]
    fn test_database_error_with_context() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE test (id INTEGER)", []).unwrap();
        let mut stmt = conn.prepare("SELECT id FROM test").unwrap();
        let result: Result<String, rusqlite::Error> = stmt.query_row([], |row| row.get(999));

        if let Err(rusqlite_err) = result {
            let err =
                TaskMgrError::database_error("/path/to/tasks.db", "querying tasks", rusqlite_err);
            let msg = err.to_string();
            assert!(msg.contains("/path/to/tasks.db"));
            assert!(msg.contains("querying tasks"));
        }
    }

    #[test]
    fn test_io_error_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: TaskMgrError = io_err.into();
        assert!(err.to_string().starts_with("I/O error:"));
    }

    #[test]
    fn test_io_error_with_context() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = TaskMgrError::io_error("/path/to/export.json", "writing export file", io_err);
        let msg = err.to_string();
        assert!(msg.contains("/path/to/export.json"));
        assert!(msg.contains("writing export file"));
        assert!(msg.contains("access denied"));
    }

    #[test]
    fn test_json_error_from() {
        let json_result: Result<i32, serde_json::Error> = serde_json::from_str("not json");
        if let Err(json_err) = json_result {
            let err: TaskMgrError = json_err.into();
            assert!(err.to_string().starts_with("JSON error:"));
        }
    }

    #[test]
    fn test_not_found_includes_resource_type_and_id() {
        // Test that NotFound errors include both resource type and task ID
        let err = TaskMgrError::task_not_found("US-001");
        if let TaskMgrError::NotFound { resource_type, id } = &err {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "US-001");
        } else {
            panic!("Expected NotFound variant");
        }
    }

    #[test]
    fn test_invalid_state_includes_all_fields() {
        // Test that InvalidState errors include expected and actual states
        let err = TaskMgrError::invalid_state("Task", "TASK-123", "todo", "done");
        if let TaskMgrError::InvalidState {
            resource_type,
            id,
            expected,
            actual,
        } = &err
        {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "TASK-123");
            assert_eq!(expected, "todo");
            assert_eq!(actual, "done");
        } else {
            panic!("Expected InvalidState variant");
        }
    }

    // Path validation tests

    #[test]
    fn test_validate_safe_path_allows_relative_paths() {
        // Valid relative paths should pass
        assert!(validate_safe_path("src/main.rs", "touchesFiles", None).is_ok());
        assert!(validate_safe_path("./src/lib.rs", "touchesFiles", None).is_ok());
        assert!(validate_safe_path("tests/fixtures/sample.json", "touchesFiles", None).is_ok());
        assert!(validate_safe_path("Cargo.toml", "touchesFiles", None).is_ok());
        assert!(validate_safe_path("deeply/nested/path/to/file.rs", "touchesFiles", None).is_ok());
    }

    #[test]
    fn test_validate_safe_path_rejects_absolute_unix_paths() {
        let result = validate_safe_path("/etc/passwd", "touchesFiles", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("absolute paths are not allowed"));
        assert!(err.contains("/etc/passwd"));

        // Unix root
        let result = validate_safe_path("/", "touchesFiles", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_safe_path_rejects_absolute_windows_paths() {
        let result = validate_safe_path("C:\\Windows\\System32", "touchesFiles", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("absolute paths are not allowed"));

        let result = validate_safe_path("D:/Users/admin", "touchesFiles", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_safe_path_rejects_parent_traversal() {
        // Classic path traversal attack
        let result = validate_safe_path("../../../etc/passwd", "touchesFiles", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("parent directory traversal"));

        // Single parent
        let result = validate_safe_path("../sibling/file.txt", "touchesFiles", None);
        assert!(result.is_err());

        // Embedded in path
        let result = validate_safe_path("foo/../bar/baz", "touchesFiles", None);
        assert!(result.is_err());

        // Windows style
        let result = validate_safe_path("foo\\..\\bar", "touchesFiles", None);
        assert!(result.is_err());

        // Just .. by itself
        let result = validate_safe_path("..", "touchesFiles", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_safe_path_rejects_home_directory() {
        let result = validate_safe_path("~/secrets/passwords.txt", "touchesFiles", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("home directory paths are not allowed"));

        let result = validate_safe_path("~", "touchesFiles", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_safe_path_rejects_unc_paths() {
        // Windows UNC paths
        let result = validate_safe_path("\\\\server\\share\\file", "touchesFiles", None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("network paths are not allowed"));

        // Unix-style network path
        let result = validate_safe_path("//server/share/file", "touchesFiles", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_safe_path_includes_task_id_in_error() {
        let result = validate_safe_path("/etc/passwd", "touchesFiles", Some("US-001"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("US-001"));
        assert!(err.contains("touchesFiles for task 'US-001'"));
    }

    #[test]
    fn test_validate_safe_path_allows_single_dot_components() {
        // ./current directory references are safe
        assert!(validate_safe_path("./file.txt", "touchesFiles", None).is_ok());
        assert!(validate_safe_path("foo/./bar/./baz.rs", "touchesFiles", None).is_ok());
    }

    #[test]
    fn test_validate_safe_path_allows_dotfiles() {
        // Dotfiles and directories starting with . are safe (not ..)
        assert!(validate_safe_path(".gitignore", "touchesFiles", None).is_ok());
        assert!(validate_safe_path(".github/workflows/ci.yml", "touchesFiles", None).is_ok());
        assert!(validate_safe_path("src/.hidden", "touchesFiles", None).is_ok());
    }

    #[test]
    fn test_unsafe_path_error_format() {
        let err = TaskMgrError::unsafe_path("touchesFiles", "../secret", "traversal not allowed");
        let msg = err.to_string();
        assert!(msg.contains("Unsafe path"));
        assert!(msg.contains("touchesFiles"));
        assert!(msg.contains("../secret"));
        assert!(msg.contains("traversal not allowed"));
    }
}
