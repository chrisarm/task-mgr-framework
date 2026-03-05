//! Environment setup and git validation for the loop engine.
//!
//! Handles .env loading, git repo validation, branch management,
//! path resolution, and directory creation. All functions return
//! `TaskMgrResult<T>` for consistent error handling.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{TaskMgrError, TaskMgrResult};

/// Prompt the user with a yes/no question on stderr and read their response from stdin.
///
/// Returns `Ok(true)` if the user answers "y" or "yes" (case-insensitive),
/// `Ok(false)` otherwise. Returns `Err` only on I/O failure.
pub(crate) fn prompt_user_yn(message: &str) -> TaskMgrResult<bool> {
    eprint!("{}", message);
    io::stderr()
        .flush()
        .map_err(|e| TaskMgrError::io_error("stderr", "flushing prompt", e))?;

    let stdin = io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| TaskMgrError::io_error("stdin", "reading user response", e))?;

    let response = line.trim().to_lowercase();
    Ok(response == "y" || response == "yes")
}

/// Load environment variables from `.env` file if present.
///
/// Uses `dotenvy::dotenv()`. Missing `.env` is not an error.
pub fn load_env() {
    dotenvy::dotenv().ok();
}

/// Validate that the given directory is inside a git repository.
///
/// Runs `git rev-parse --git-dir` to check for a valid git repo.
///
/// # Errors
///
/// Returns an error if the directory is not inside a git repository
/// or if the `git` command is not found.
pub fn validate_git_repo(dir: &Path) -> TaskMgrResult<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(dir.display().to_string(), "running git rev-parse", e)
        })?;

    if !output.status.success() {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git repository".to_string(),
            id: dir.display().to_string(),
            expected: "a git repository".to_string(),
            actual: "not a git repository. Run task-mgr from within a git repo, or run 'git init' first.".to_string(),
        });
    }

    Ok(())
}

/// Get the current git branch name.
///
/// Runs `git branch --show-current`.
///
/// # Errors
///
/// Returns an error if the git command fails or the output is not valid UTF-8.
pub fn get_current_branch(dir: &Path) -> TaskMgrResult<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                dir.display().to_string(),
                "running git branch --show-current",
                e,
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git branch".to_string(),
            id: dir.display().to_string(),
            expected: "a branch name".to_string(),
            actual: format!("git error: {}", stderr.trim()),
        });
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(branch)
}

/// Check for uncommitted changes and prompt the user if dirty.
///
/// Runs `git status --porcelain`. If there are uncommitted changes:
/// - In `yes_mode`: logs a warning to stderr and continues
/// - In interactive mode: prompts the user to continue or abort
///
/// # Errors
///
/// Returns an error if the user declines to continue or if git commands fail.
pub fn check_uncommitted_changes(dir: &Path, yes_mode: bool) -> TaskMgrResult<()> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .map_err(|e| TaskMgrError::io_error(dir.display().to_string(), "running git status", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git status".to_string(),
            id: dir.display().to_string(),
            expected: "clean git status check".to_string(),
            actual: format!("git error: {}", stderr.trim()),
        });
    }

    let status_output = String::from_utf8_lossy(&output.stdout);
    let status_output = status_output.trim();

    if status_output.is_empty() {
        return Ok(());
    }

    // Count changed files
    let changed_count = status_output.lines().count();

    if yes_mode {
        eprintln!(
            "Warning: {} uncommitted change(s) detected. Continuing in --yes mode.",
            changed_count
        );
        return Ok(());
    }

    eprintln!("Warning: {} uncommitted change(s) detected:", changed_count);
    // Show at most 10 lines of changes
    for line in status_output.lines().take(10) {
        eprintln!("  {}", line);
    }
    if changed_count > 10 {
        eprintln!("  ... and {} more", changed_count - 10);
    }
    if prompt_user_yn("Continue with uncommitted changes? [y/N] ")? {
        Ok(())
    } else {
        Err(TaskMgrError::InvalidState {
            resource_type: "User confirmation".to_string(),
            id: "uncommitted changes".to_string(),
            expected: "user approved continuation".to_string(),
            actual: "user declined".to_string(),
        })
    }
}

/// Ensure the working directory is on the correct git branch.
///
/// If the current branch doesn't match `branch_name`:
/// - Tries to check out the branch
/// - If the branch doesn't exist, creates it
///
/// In interactive mode, prompts before switching. In `yes_mode`, switches automatically.
///
/// # Errors
///
/// Returns an error if branch checkout/creation fails or the user declines.
pub fn ensure_branch(dir: &Path, branch_name: &str, yes_mode: bool) -> TaskMgrResult<()> {
    let current = get_current_branch(dir)?;

    if current == branch_name {
        return Ok(());
    }

    if !yes_mode {
        eprintln!(
            "Current branch '{}' does not match PRD branch '{}'.",
            current, branch_name
        );
        if !prompt_user_yn(&format!("Switch to '{}' ? [y/N] ", branch_name))? {
            return Err(TaskMgrError::InvalidState {
                resource_type: "User confirmation".to_string(),
                id: "branch switch".to_string(),
                expected: "user approved branch switch".to_string(),
                actual: "user declined".to_string(),
            });
        }
    } else {
        eprintln!(
            "Switching from branch '{}' to '{}'...",
            current, branch_name
        );
    }

    // Check if branch already exists using git rev-parse --verify
    let branch_exists = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", branch_name),
        ])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            TaskMgrError::io_error(
                dir.display().to_string(),
                "running git rev-parse --verify",
                e,
            )
        })?
        .success();

    if branch_exists {
        // Branch exists — attempt checkout
        let checkout = Command::new("git")
            .args(["checkout", branch_name])
            .current_dir(dir)
            .output()
            .map_err(|e| {
                TaskMgrError::io_error(dir.display().to_string(), "running git checkout", e)
            })?;

        if checkout.status.success() {
            eprintln!("Switched to branch '{}'", branch_name);
            return Ok(());
        }

        // Checkout failed — surface the real error with actionable hint
        let stderr = String::from_utf8_lossy(&checkout.stderr);
        let hint = if stderr.contains("would be overwritten") {
            "Commit or stash your changes first: git stash"
        } else {
            "Check `git status` for details."
        };

        return Err(TaskMgrError::InvalidState {
            resource_type: "Git branch".to_string(),
            id: branch_name.to_string(),
            expected: "successful branch checkout".to_string(),
            actual: format!("{}. {}", stderr.trim(), hint),
        });
    }

    // Branch doesn't exist — create it
    let create = Command::new("git")
        .args(["checkout", "-b", branch_name])
        .current_dir(dir)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(dir.display().to_string(), "running git checkout -b", e)
        })?;

    if create.status.success() {
        eprintln!("Created and switched to new branch '{}'", branch_name);
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&create.stderr);
    Err(TaskMgrError::InvalidState {
        resource_type: "Git branch".to_string(),
        id: branch_name.to_string(),
        expected: "successful branch creation".to_string(),
        actual: format!("git error: {}", stderr.trim()),
    })
}

/// Resolved paths for loop engine operation.
#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    /// Absolute path to the PRD JSON file
    pub prd_file: PathBuf,
    /// Absolute path to the prompt file
    pub prompt_file: PathBuf,
    /// Absolute path to the progress file
    pub progress_file: PathBuf,
    /// Absolute path to the tasks directory
    pub tasks_dir: PathBuf,
}

/// Resolve relative paths to absolute paths.
///
/// - `prd_file` must exist (error if not found)
/// - `prompt_file`: if `None`, derived from PRD filename by replacing `.json` with `-prompt.md`
/// - `progress_file` defaults to `tasks/progress.txt`; when `prefix` is `Some("P1")` it becomes
///   `tasks/progress-P1.txt` (per-PRD progress isolation)
///
/// # Errors
///
/// Returns an error if the PRD file does not exist or path resolution fails.
pub fn resolve_paths(
    prd_file: &Path,
    prompt_file: Option<&Path>,
    project_dir: &Path,
    prefix: Option<&str>,
) -> TaskMgrResult<ResolvedPaths> {
    // Resolve PRD file to absolute path
    let prd_absolute = if prd_file.is_absolute() {
        prd_file.to_path_buf()
    } else {
        project_dir.join(prd_file)
    };

    if !prd_absolute.exists() {
        return Err(TaskMgrError::NotFound {
            resource_type: "PRD file".to_string(),
            id: format!(
                "{}\n\nHint: Check the path relative to your project root ({}).",
                prd_absolute.display(),
                project_dir.display()
            ),
        });
    }

    let prd_absolute = prd_absolute.canonicalize().map_err(|e| {
        TaskMgrError::io_error(prd_absolute.display().to_string(), "resolving PRD path", e)
    })?;

    // Resolve prompt file
    let prompt_absolute = match prompt_file {
        Some(p) => {
            let resolved = if p.is_absolute() {
                p.to_path_buf()
            } else {
                project_dir.join(p)
            };
            if resolved.exists() {
                resolved.canonicalize().map_err(|e| {
                    TaskMgrError::io_error(
                        resolved.display().to_string(),
                        "resolving prompt path",
                        e,
                    )
                })?
            } else {
                resolved
            }
        }
        None => {
            // Derive from PRD filename: foo.json -> foo-prompt.md
            let stem = prd_absolute
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let prompt_name = format!("{}-prompt.md", stem);
            let prompt_path = prd_absolute
                .parent()
                .unwrap_or(&prd_absolute)
                .join(prompt_name);
            prompt_path
        }
    };

    // Resolve progress file — use prefix to isolate per-PRD progress when provided
    let progress_filename = match prefix {
        Some(p) => format!("progress-{}.txt", p),
        None => "progress.txt".to_string(),
    };
    let progress_file = project_dir.join("tasks").join(progress_filename);

    // Derive tasks directory from PRD location
    let tasks_dir = prd_absolute.parent().unwrap_or(project_dir).to_path_buf();

    Ok(ResolvedPaths {
        prd_file: prd_absolute,
        prompt_file: prompt_absolute,
        progress_file,
        tasks_dir,
    })
}

/// Ensure required directories exist, creating them if necessary.
///
/// Creates:
/// - `tasks/` directory
/// - `tasks/archive/` directory
///
/// # Errors
///
/// Returns an error if directory creation fails.
pub fn ensure_directories(project_dir: &Path) -> TaskMgrResult<()> {
    let tasks_dir = project_dir.join("tasks");
    let archive_dir = tasks_dir.join("archive");

    if !tasks_dir.exists() {
        std::fs::create_dir_all(&tasks_dir).map_err(|e| {
            TaskMgrError::io_error(
                tasks_dir.display().to_string(),
                "creating tasks directory",
                e,
            )
        })?;
    }

    if !archive_dir.exists() {
        std::fs::create_dir_all(&archive_dir).map_err(|e| {
            TaskMgrError::io_error(
                archive_dir.display().to_string(),
                "creating archive directory",
                e,
            )
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- load_env ---

    #[test]
    fn test_load_env_does_not_panic_without_dotenv() {
        // Should not panic even if no .env file exists
        load_env();
    }

    // --- validate_git_repo ---

    #[test]
    fn test_validate_git_repo_succeeds_in_git_repo() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(validate_git_repo(project_root).is_ok());
    }

    #[test]
    fn test_validate_git_repo_fails_outside_git_repo() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let result = validate_git_repo(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not a git repository"),
            "Expected 'not a git repository', got: {}",
            err
        );
    }

    // --- get_current_branch ---

    #[test]
    fn test_get_current_branch_returns_nonempty_string() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let branch = get_current_branch(project_root).expect("should get branch");
        assert!(!branch.is_empty(), "Branch name should not be empty");
    }

    #[test]
    fn test_get_current_branch_fails_outside_git_repo() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let result = get_current_branch(tmp.path());
        assert!(result.is_err());
    }

    // --- resolve_paths ---

    #[test]
    fn test_resolve_paths_with_existing_prd() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let resolved = resolve_paths(&prd, None, tmp.path(), None).expect("resolve paths");
        assert!(resolved.prd_file.is_absolute());
        assert!(resolved.prd_file.exists());
        // Prompt derived from PRD name
        assert!(
            resolved
                .prompt_file
                .to_string_lossy()
                .contains("test-prompt.md"),
            "Prompt file should be derived: {:?}",
            resolved.prompt_file
        );
    }

    #[test]
    fn test_resolve_paths_nonexistent_prd_returns_error() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("nonexistent.json");

        let result = resolve_paths(&prd, None, tmp.path(), None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "Expected 'not found', got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_paths_with_explicit_prompt_file() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        let prompt = tmp.path().join("custom-prompt.md");
        fs::write(&prd, "{}").expect("write prd");
        fs::write(&prompt, "# Prompt").expect("write prompt");

        let resolved = resolve_paths(&prd, Some(&prompt), tmp.path(), None).expect("resolve paths");
        assert!(resolved.prompt_file.is_absolute());
        assert!(resolved
            .prompt_file
            .to_string_lossy()
            .contains("custom-prompt.md"),);
    }

    #[test]
    fn test_resolve_paths_relative_prd() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");
        let prd = tasks_dir.join("my-prd.json");
        fs::write(&prd, "{}").expect("write prd");

        // Pass relative path
        let relative = Path::new("tasks").join("my-prd.json");
        let resolved = resolve_paths(&relative, None, tmp.path(), None).expect("resolve paths");
        assert!(resolved.prd_file.is_absolute());
    }

    #[test]
    fn test_resolve_paths_progress_file_location() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let resolved = resolve_paths(&prd, None, tmp.path(), None).expect("resolve paths");
        assert!(resolved
            .progress_file
            .to_string_lossy()
            .contains("progress.txt"),);
    }

    #[test]
    fn test_resolve_paths_with_prefix_returns_named_progress_file() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let resolved =
            resolve_paths(&prd, None, tmp.path(), Some("P1")).expect("resolve paths with prefix");
        let progress = resolved.progress_file.to_string_lossy();
        assert!(
            progress.contains("progress-P1.txt"),
            "Expected progress-P1.txt, got: {}",
            progress
        );
        assert!(
            !progress.ends_with("progress.txt") || progress.contains("progress-P1.txt"),
            "Should use prefix-specific filename"
        );
    }

    #[test]
    fn test_resolve_paths_different_prefixes_produce_different_files() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let no_prefix = resolve_paths(&prd, None, tmp.path(), None).expect("no prefix");
        let with_prefix = resolve_paths(&prd, None, tmp.path(), Some("ABC")).expect("with prefix");

        assert_ne!(
            no_prefix.progress_file, with_prefix.progress_file,
            "Different prefix should yield different progress files"
        );
        assert!(no_prefix
            .progress_file
            .to_string_lossy()
            .ends_with("progress.txt"));
        assert!(with_prefix
            .progress_file
            .to_string_lossy()
            .ends_with("progress-ABC.txt"));
    }

    // --- ensure_directories ---

    #[test]
    fn test_ensure_directories_creates_tasks_and_archive() {
        let tmp = tempfile::tempdir().expect("create temp dir");

        ensure_directories(tmp.path()).expect("ensure directories");

        assert!(tmp.path().join("tasks").exists());
        assert!(tmp.path().join("tasks").join("archive").exists());
    }

    #[test]
    fn test_ensure_directories_idempotent() {
        let tmp = tempfile::tempdir().expect("create temp dir");

        ensure_directories(tmp.path()).expect("first call");
        ensure_directories(tmp.path()).expect("second call should also succeed");

        assert!(tmp.path().join("tasks").exists());
        assert!(tmp.path().join("tasks").join("archive").exists());
    }

    #[test]
    fn test_ensure_directories_preserves_existing_content() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");
        let existing_file = tasks_dir.join("existing.txt");
        fs::write(&existing_file, "content").expect("write file");

        ensure_directories(tmp.path()).expect("ensure directories");

        assert!(existing_file.exists());
        assert_eq!(fs::read_to_string(&existing_file).expect("read"), "content");
    }

    // --- check_uncommitted_changes (yes_mode only, interactive needs stdin) ---

    #[test]
    fn test_check_uncommitted_changes_clean_repo() {
        // Create a clean git repo
        let tmp = tempfile::tempdir().expect("create temp dir");
        Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .expect("git config name");

        // Make an initial commit so status is clean
        fs::write(tmp.path().join("README.md"), "# Test").expect("write readme");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        let result = check_uncommitted_changes(tmp.path(), true);
        assert!(result.is_ok(), "Clean repo should pass: {:?}", result.err());
    }

    #[test]
    fn test_check_uncommitted_changes_dirty_repo_yes_mode() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .expect("git config name");

        // Create untracked file
        fs::write(tmp.path().join("dirty.txt"), "uncommitted").expect("write file");

        // In yes_mode, should succeed even with dirty repo
        let result = check_uncommitted_changes(tmp.path(), true);
        assert!(
            result.is_ok(),
            "yes_mode should allow dirty repo: {:?}",
            result.err()
        );
    }

    // --- ensure_branch (requires git repo) ---

    #[test]
    fn test_ensure_branch_already_on_correct_branch() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .expect("git config name");
        fs::write(tmp.path().join("file.txt"), "content").expect("write");
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        let result = ensure_branch(tmp.path(), "main", true);
        assert!(
            result.is_ok(),
            "Should succeed when already on correct branch: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_ensure_branch_creates_new_branch_yes_mode() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .expect("git config name");
        fs::write(tmp.path().join("file.txt"), "content").expect("write");
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        let result = ensure_branch(tmp.path(), "feature/new-branch", true);
        assert!(
            result.is_ok(),
            "Should create new branch: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "feature/new-branch");
    }

    #[test]
    fn test_ensure_branch_switches_to_existing_branch_yes_mode() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .expect("git config name");
        fs::write(tmp.path().join("file.txt"), "content").expect("write");
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        // Create a branch and switch back to main
        Command::new("git")
            .args(["branch", "feature/existing"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        let result = ensure_branch(tmp.path(), "feature/existing", true);
        assert!(
            result.is_ok(),
            "Should switch to existing branch: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "feature/existing");
    }

    #[test]
    fn test_check_uncommitted_changes_fails_outside_git_repo() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let result = check_uncommitted_changes(tmp.path(), true);
        assert!(result.is_err(), "Should fail outside git repo");
    }

    // --- TEST-INIT-002: ensure_branch with dirty tree and edge cases ---

    fn setup_git_repo() -> tempfile::TempDir {
        crate::loop_engine::test_utils::setup_git_repo()
    }

    #[test]
    fn test_ensure_branch_dirty_tree_error_mentions_stash() {
        let tmp = setup_git_repo();

        // Create a branch with a different version of file.txt so checkout conflicts
        Command::new("git")
            .args(["checkout", "-b", "feature/target"])
            .current_dir(tmp.path())
            .output()
            .expect("create target branch");
        fs::write(tmp.path().join("file.txt"), "branch content").expect("write branch content");
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "branch change"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        // Switch back to main and dirty the tracked file
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout main");
        fs::write(tmp.path().join("file.txt"), "dirty local change").expect("dirty tracked file");

        // Try to switch to feature/target — should fail because tree is dirty
        let result = ensure_branch(tmp.path(), "feature/target", true);

        // After FIX-006: error should mention stash/git status
        assert!(result.is_err(), "Should fail when tree is dirty");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("stash") || err.contains("git status"),
            "Error should mention 'stash' or 'git status' for actionable guidance, got: {}",
            err
        );
    }

    #[test]
    fn test_ensure_branch_dirty_tree_returns_error() {
        // Verify that ensure_branch returns an error when the tree is dirty
        // and switching branches would overwrite uncommitted changes.
        // (The error message quality is tested by the #[ignore] test above,
        // this test just verifies the error is returned, not swallowed.)
        let tmp = setup_git_repo();

        // Create a branch with a different version of file.txt
        Command::new("git")
            .args(["checkout", "-b", "feature/target"])
            .current_dir(tmp.path())
            .output()
            .expect("create target branch");
        fs::write(tmp.path().join("file.txt"), "branch content").expect("write branch content");
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "branch change"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        // Switch back to main
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout main");

        // Make file.txt dirty on main (conflicts with feature/target version)
        fs::write(tmp.path().join("file.txt"), "dirty local change").expect("dirty tracked file");

        // Try to switch to feature/target — should fail because local changes
        // to file.txt would be overwritten by the checkout
        let result = ensure_branch(tmp.path(), "feature/target", true);

        // ensure_branch detects dirty tree and gives actionable error
        // This MUST be an error, not a silent success
        assert!(
            result.is_err(),
            "Should fail when tree has uncommitted changes that conflict with checkout"
        );
    }

    #[test]
    fn test_ensure_branch_already_on_correct_branch_with_dirty_tree() {
        // When already on the correct branch, ensure_branch should succeed
        // even if the tree is dirty (no checkout needed).
        let tmp = setup_git_repo();

        // Dirty the tree
        fs::write(tmp.path().join("file.txt"), "dirty content").expect("dirty tracked file");

        // Already on 'main', request 'main' — should succeed (no-op)
        let result = ensure_branch(tmp.path(), "main", true);
        assert!(
            result.is_ok(),
            "Should succeed when already on correct branch even with dirty tree: {:?}",
            result.err()
        );
    }

    // --- TEST-INIT-001: project_root vs db_dir separation ---

    #[test]
    fn test_resolve_paths_with_project_root_resolves_prd_relative_to_project_root() {
        // Simulate project_root and db_dir being different directories.
        // PRD lives under project_root/tasks/, resolve_paths should find it
        // when project_root is passed as project_dir.
        let project_root = tempfile::tempdir().expect("create project root");
        let db_dir = tempfile::tempdir().expect("create db dir");

        // Create PRD under project_root
        let tasks_dir = project_root.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");
        let prd = tasks_dir.join("my-prd.json");
        fs::write(&prd, r#"{"version":"1.0"}"#).expect("write prd");

        // Pass relative PRD path and project_root as project_dir (NOT db_dir)
        let relative_prd = Path::new("tasks").join("my-prd.json");
        let resolved = resolve_paths(&relative_prd, None, project_root.path(), None)
            .expect("should resolve relative to project_root");

        assert!(resolved.prd_file.is_absolute());
        assert!(resolved.prd_file.exists());

        // Verify it's under project_root, not db_dir
        assert!(
            resolved
                .prd_file
                .starts_with(project_root.path().canonicalize().unwrap()),
            "PRD should resolve under project_root ({:?}), not db_dir ({:?}). Got: {:?}",
            project_root.path(),
            db_dir.path(),
            resolved.prd_file
        );
    }

    #[test]
    fn test_resolve_paths_with_project_root_different_from_db_dir() {
        // When project_root != db_dir, PRD path should still resolve via project_root
        let project_root = tempfile::tempdir().expect("create project root");
        let db_dir = tempfile::tempdir().expect("create db dir");

        // PRD exists under project_root but NOT under db_dir
        let prd = project_root.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        // Resolving with project_root should succeed
        let resolved = resolve_paths(&prd, None, project_root.path(), None)
            .expect("should find PRD under project_root");
        assert!(resolved.prd_file.exists());

        // Resolving same relative path with db_dir should fail
        // (because the PRD doesn't exist under db_dir)
        let relative = Path::new("test.json");
        let result = resolve_paths(relative, None, db_dir.path(), None);
        assert!(
            result.is_err(),
            "PRD should NOT be found under db_dir when it only exists under project_root"
        );
    }

    #[test]
    fn test_resolve_paths_progress_file_under_project_root() {
        // progress.txt should be resolved relative to project_root (project_dir param)
        let project_root = tempfile::tempdir().expect("create project root");
        let prd = project_root.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let resolved = resolve_paths(&prd, None, project_root.path(), None).expect("resolve paths");

        // progress.txt should be under project_root/tasks/progress.txt
        let expected_progress = project_root.path().join("tasks").join("progress.txt");
        assert_eq!(
            resolved.progress_file, expected_progress,
            "progress.txt should be under project_root/tasks/"
        );
    }

    #[test]
    fn test_ensure_directories_creates_tasks_under_project_root_not_db_dir() {
        // When project_root is passed, tasks/ should be created under project_root
        let project_root = tempfile::tempdir().expect("create project root");
        let db_dir = tempfile::tempdir().expect("create db dir");

        // Call with project_root
        ensure_directories(project_root.path()).expect("ensure directories under project_root");

        // Verify tasks/ exists under project_root
        assert!(
            project_root.path().join("tasks").exists(),
            "tasks/ should exist under project_root"
        );
        assert!(
            project_root.path().join("tasks").join("archive").exists(),
            "tasks/archive/ should exist under project_root"
        );

        // Verify tasks/ was NOT created under db_dir
        assert!(
            !db_dir.path().join("tasks").exists(),
            "tasks/ should NOT exist under db_dir (was not the target)"
        );
    }

    // --- TEST-001: Comprehensive project root separation edge cases ---

    #[test]
    fn test_resolve_paths_absolute_prd_ignores_project_root() {
        // An absolute PRD path should resolve correctly regardless of project_root.
        let project_root = tempfile::tempdir().expect("create project root");
        let other_dir = tempfile::tempdir().expect("create other dir");

        // Create PRD under other_dir (not project_root)
        let prd = other_dir.path().join("my.json");
        fs::write(&prd, "{}").expect("write prd");

        // Pass absolute PRD path — project_root shouldn't matter for resolution
        let resolved = resolve_paths(&prd, None, project_root.path(), None)
            .expect("absolute PRD should resolve regardless of project_root");

        assert!(resolved.prd_file.is_absolute());
        assert!(resolved.prd_file.exists());
        // It should be under other_dir, not project_root
        assert!(
            resolved
                .prd_file
                .starts_with(other_dir.path().canonicalize().unwrap()),
            "Absolute PRD should stay where it is, not be rebased to project_root"
        );
    }

    #[test]
    fn test_resolve_paths_relative_prd_nested_subdirectory() {
        // PRD at tasks/sub/deep.json should resolve relative to project_root
        let project_root = tempfile::tempdir().expect("create project root");
        let nested = project_root.path().join("tasks").join("sub");
        fs::create_dir_all(&nested).expect("create nested dir");
        let prd = nested.join("deep.json");
        fs::write(&prd, r#"{"version":"1.0"}"#).expect("write prd");

        let relative = Path::new("tasks").join("sub").join("deep.json");
        let resolved = resolve_paths(&relative, None, project_root.path(), None)
            .expect("nested relative PRD should resolve");

        assert!(resolved.prd_file.is_absolute());
        assert!(resolved.prd_file.exists());

        // Prompt file should be derived from the PRD name in the same directory
        assert!(
            resolved
                .prompt_file
                .to_string_lossy()
                .contains("deep-prompt.md"),
            "Prompt should be derived from nested PRD name: {:?}",
            resolved.prompt_file
        );
    }

    #[test]
    fn test_resolve_paths_explicit_prompt_relative_to_project_root() {
        // An explicit prompt file given as a relative path should resolve
        // relative to project_root, not CWD.
        let project_root = tempfile::tempdir().expect("create project root");
        let tasks = project_root.path().join("tasks");
        fs::create_dir_all(&tasks).expect("create tasks dir");

        let prd = tasks.join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let prompt_file = tasks.join("custom-prompt.md");
        fs::write(&prompt_file, "# Custom").expect("write prompt");

        // Pass relative paths for both PRD and prompt
        let relative_prd = Path::new("tasks").join("test.json");
        let relative_prompt = Path::new("tasks").join("custom-prompt.md");
        let resolved = resolve_paths(
            &relative_prd,
            Some(&relative_prompt),
            project_root.path(),
            None,
        )
        .expect("should resolve both relative paths");

        assert!(resolved.prd_file.is_absolute());
        assert!(resolved.prompt_file.is_absolute());
        assert!(
            resolved
                .prompt_file
                .starts_with(project_root.path().canonicalize().unwrap()),
            "Prompt file should resolve under project_root"
        );
    }

    #[test]
    fn test_resolve_paths_tasks_dir_derived_from_prd_location() {
        // tasks_dir should be derived from the PRD file's parent, not project_root
        let project_root = tempfile::tempdir().expect("create project root");
        let custom_dir = project_root.path().join("custom");
        fs::create_dir_all(&custom_dir).expect("create custom dir");
        let prd = custom_dir.join("my-prd.json");
        fs::write(&prd, "{}").expect("write prd");

        let relative = Path::new("custom").join("my-prd.json");
        let resolved =
            resolve_paths(&relative, None, project_root.path(), None).expect("resolve paths");

        // tasks_dir should be the parent of the PRD file (custom/), not project_root
        assert!(
            resolved.tasks_dir.ends_with("custom"),
            "tasks_dir should be derived from PRD parent directory, got: {:?}",
            resolved.tasks_dir
        );
    }

    #[test]
    fn test_resolve_paths_parameterized_absolute_relative_combinations() {
        // Test all 4 combinations: (abs PRD, rel PRD) × (project_root same as PRD parent, different)
        let root1 = tempfile::tempdir().expect("root1");
        let root2 = tempfile::tempdir().expect("root2");

        // Setup: PRD under root1/tasks/
        let tasks1 = root1.path().join("tasks");
        fs::create_dir_all(&tasks1).expect("mkdir tasks1");
        let prd_path = tasks1.join("test.json");
        fs::write(&prd_path, "{}").expect("write prd");

        // Case 1: Absolute PRD + project_root == PRD parent tree → OK
        let r = resolve_paths(&prd_path, None, root1.path(), None);
        assert!(r.is_ok(), "Abs PRD + matching project_root: {:?}", r.err());

        // Case 2: Absolute PRD + project_root != PRD parent → OK (absolute is self-contained)
        let r = resolve_paths(&prd_path, None, root2.path(), None);
        assert!(r.is_ok(), "Abs PRD + different project_root: {:?}", r.err());

        // Case 3: Relative PRD + correct project_root → OK
        let rel = Path::new("tasks").join("test.json");
        let r = resolve_paths(&rel, None, root1.path(), None);
        assert!(r.is_ok(), "Rel PRD + correct project_root: {:?}", r.err());

        // Case 4: Relative PRD + wrong project_root → FAIL (PRD not under root2)
        let r = resolve_paths(&rel, None, root2.path(), None);
        assert!(
            r.is_err(),
            "Rel PRD + wrong project_root should fail because PRD doesn't exist there"
        );
    }

    #[test]
    fn test_ensure_directories_no_effect_on_sibling_dirs() {
        // ensure_directories(project_root) should NOT create anything outside project_root
        let project_root = tempfile::tempdir().expect("project root");
        let sibling = tempfile::tempdir().expect("sibling dir");

        ensure_directories(project_root.path()).expect("ensure dirs");

        // Verify project_root has tasks/ and tasks/archive/
        assert!(project_root.path().join("tasks").exists());
        assert!(project_root.path().join("tasks").join("archive").exists());

        // Verify sibling is untouched (no tasks/ created)
        assert!(
            !sibling.path().join("tasks").exists(),
            "Sibling directory should not be affected"
        );
    }

    // --- TEST-002: Comprehensive tests for ensure_branch error handling ---

    #[test]
    fn test_ensure_branch_deeply_nested_slash_branch_name() {
        // Branch names like 'feat/user/auth-v2' with multiple slashes should work.
        let tmp = setup_git_repo();

        let result = ensure_branch(tmp.path(), "feat/user/auth-v2", true);
        assert!(
            result.is_ok(),
            "Deeply nested branch name with slashes should be created: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "feat/user/auth-v2");
    }

    #[test]
    fn test_ensure_branch_switch_to_existing_slashed_branch() {
        // Switch to an existing branch with slashes in the name.
        let tmp = setup_git_repo();

        // Create the target branch and switch back
        Command::new("git")
            .args(["branch", "release/v1.2/hotfix"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        let result = ensure_branch(tmp.path(), "release/v1.2/hotfix", true);
        assert!(
            result.is_ok(),
            "Switch to existing slashed branch should work: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "release/v1.2/hotfix");
    }

    #[test]
    fn test_ensure_branch_detached_head_creates_branch() {
        // In detached HEAD state, `git branch --show-current` returns empty string.
        // ensure_branch should detect this as "not on the target branch" and
        // either switch to it (if it exists) or create it.
        let tmp = setup_git_repo();

        // Create a detached HEAD by checking out a commit directly
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(tmp.path())
            .output()
            .expect("get HEAD sha");
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Command::new("git")
            .args(["checkout", &sha])
            .current_dir(tmp.path())
            .stderr(std::process::Stdio::null())
            .output()
            .expect("detach HEAD");

        // Verify we're in detached state
        let current = get_current_branch(tmp.path()).expect("get branch");
        assert!(
            current.is_empty(),
            "Should be in detached HEAD (empty branch name), got: '{}'",
            current
        );

        // ensure_branch should create a new branch from detached HEAD
        let result = ensure_branch(tmp.path(), "recovery-branch", true);
        assert!(
            result.is_ok(),
            "Should create branch from detached HEAD: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "recovery-branch");
    }

    #[test]
    fn test_ensure_branch_detached_head_switches_to_existing() {
        // Detached HEAD + existing target branch should checkout the existing branch.
        let tmp = setup_git_repo();

        // Create a target branch
        Command::new("git")
            .args(["branch", "target-branch"])
            .current_dir(tmp.path())
            .output()
            .expect("create target branch");

        // Detach HEAD
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(tmp.path())
            .output()
            .expect("get HEAD sha");
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Command::new("git")
            .args(["checkout", &sha])
            .current_dir(tmp.path())
            .stderr(std::process::Stdio::null())
            .output()
            .expect("detach HEAD");

        // ensure_branch should switch to existing branch
        let result = ensure_branch(tmp.path(), "target-branch", true);
        assert!(
            result.is_ok(),
            "Should switch to existing branch from detached HEAD: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "target-branch");
    }

    #[test]
    fn test_ensure_branch_dirty_tree_error_format() {
        // Verify the specific error structure for dirty-tree checkout failure:
        // - Error type is InvalidState
        // - resource_type is "Git branch"
        // - Contains the target branch name
        // - Contains git's stderr message about overwriting
        // - Contains actionable hint
        let tmp = setup_git_repo();

        // Create a branch with divergent file.txt
        Command::new("git")
            .args(["checkout", "-b", "diverged"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");
        fs::write(tmp.path().join("file.txt"), "diverged content").expect("write");
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "diverge"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");

        // Switch back to main, dirty the file
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout main");
        fs::write(tmp.path().join("file.txt"), "local dirty").expect("dirty");

        let result = ensure_branch(tmp.path(), "diverged", true);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_str = err.to_string();

        // Verify error contains branch name
        assert!(
            err_str.contains("diverged"),
            "Error should contain target branch name 'diverged', got: {}",
            err_str
        );

        // Verify it contains the "would be overwritten" git message
        assert!(
            err_str.contains("would be overwritten"),
            "Error should contain git's 'would be overwritten' message, got: {}",
            err_str
        );

        // Verify actionable hint about stashing
        assert!(
            err_str.contains("stash"),
            "Error should contain 'stash' hint, got: {}",
            err_str
        );
    }

    #[test]
    fn test_ensure_branch_non_conflicting_dirty_tree_succeeds() {
        // When the dirty file does NOT conflict with the target branch,
        // git checkout succeeds even with a dirty tree.
        let tmp = setup_git_repo();

        // Create branch with the SAME file.txt content (no divergence)
        Command::new("git")
            .args(["branch", "no-conflict"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        // Dirty a DIFFERENT file (untracked)
        fs::write(tmp.path().join("new-untracked.txt"), "untracked").expect("write");

        let result = ensure_branch(tmp.path(), "no-conflict", true);
        assert!(
            result.is_ok(),
            "Non-conflicting dirty tree should allow checkout: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "no-conflict");
    }

    #[test]
    fn test_ensure_branch_branch_name_with_special_chars() {
        // Branch names with hyphens, underscores, and dots should work.
        let tmp = setup_git_repo();

        let result = ensure_branch(tmp.path(), "fix_bug-123.hotfix", true);
        assert!(
            result.is_ok(),
            "Branch name with hyphens/underscores/dots should work: {:?}",
            result.err()
        );

        let current = get_current_branch(tmp.path()).expect("get branch");
        assert_eq!(current, "fix_bug-123.hotfix");
    }

    #[test]
    #[ignore = "blocks on stdin read — requires interactive terminal"]
    fn test_ensure_branch_interactive_mode_declined_returns_error() {
        // In interactive mode (yes_mode=false), prompt_user_yn reads from stdin,
        // which blocks indefinitely in non-interactive test environments.
        let tmp = setup_git_repo();

        // Create a branch so there's something to switch to
        Command::new("git")
            .args(["branch", "other-branch"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        // In a test environment with no interactive stdin, yes_mode=false should
        // either fail with I/O error or return user-declined error.
        let result = ensure_branch(tmp.path(), "other-branch", false);

        // Either way, it should NOT succeed (no interactive approval given)
        // The error could be I/O-related or user-declined
        assert!(
            result.is_err(),
            "Interactive mode without stdin should fail: {:?}",
            result.ok()
        );
    }

    #[test]
    fn test_ensure_branch_returns_ok_when_no_checkout_needed() {
        // Calling ensure_branch when already on the correct branch
        // should return Ok(()) without doing any git operations beyond checking current branch.
        let tmp = setup_git_repo();

        // Already on 'main' and asking for 'main'
        let result = ensure_branch(tmp.path(), "main", true);
        assert!(result.is_ok());

        // Also works in interactive mode — no prompt needed when already on correct branch
        let result = ensure_branch(tmp.path(), "main", false);
        assert!(
            result.is_ok(),
            "Interactive mode should succeed with no-op (already on correct branch): {:?}",
            result.err()
        );
    }

    // --- TEST-INIT-003: actionable error messages ---
    // Note: test_ensure_branch_dirty_tree_error_mentions_stash (from TEST-INIT-002 above)
    // covers the ensure_branch acceptance criterion for this story.

    #[test]
    fn test_validate_git_repo_error_contains_git_init_hint() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let result = validate_git_repo(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("git init"),
            "validate_git_repo error should mention 'git init' as actionable hint, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_paths_not_found_error_contains_project_root_hint() {
        let project_root = tempfile::tempdir().expect("create project root");
        let nonexistent_prd = Path::new("tasks").join("nonexistent.json");

        let result = resolve_paths(&nonexistent_prd, None, project_root.path(), None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();

        // Error should include an explicit hint with the project root path
        // (not just the path as part of the NotFound id)
        let err_lower = err.to_lowercase();
        assert!(
            err_lower.contains("hint") || err_lower.contains("project root"),
            "resolve_paths not-found error should contain 'Hint' or 'project root' \
             for actionable guidance, got: {}",
            err
        );
    }

    // --- TEST-INIT-003: resolve_paths() prefix parameter ---

    #[test]
    fn test_resolve_paths_without_prefix_returns_progress_txt() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let resolved = resolve_paths(&prd, None, tmp.path(), None).expect("resolve paths");
        assert!(
            resolved
                .progress_file
                .to_string_lossy()
                .ends_with("progress.txt"),
            "Without prefix, progress file should be 'progress.txt', got: {:?}",
            resolved.progress_file
        );
    }

    #[test]
    fn test_resolve_paths_with_prefix_returns_prefixed_progress() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let resolved = resolve_paths(&prd, None, tmp.path(), Some("P1")).expect("resolve paths");
        assert!(
            resolved
                .progress_file
                .to_string_lossy()
                .ends_with("progress-P1.txt"),
            "With prefix 'P1', progress file should be 'progress-P1.txt', got: {:?}",
            resolved.progress_file
        );
    }

    /// Known-bad discriminator: prefix must produce different filename than no-prefix
    #[test]
    fn test_resolve_paths_prefix_p1_differs_from_no_prefix() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let prd = tmp.path().join("test.json");
        fs::write(&prd, "{}").expect("write prd");

        let without_prefix = resolve_paths(&prd, None, tmp.path(), None).expect("no prefix");
        let with_prefix = resolve_paths(&prd, None, tmp.path(), Some("P1")).expect("with prefix");

        assert_ne!(
            without_prefix.progress_file, with_prefix.progress_file,
            "prefix='P1' must produce 'progress-P1.txt', not 'progress.txt'"
        );
        assert!(
            with_prefix
                .progress_file
                .to_string_lossy()
                .contains("progress-P1.txt"),
            "progress file with P1 prefix must contain 'progress-P1.txt'"
        );
        assert!(
            without_prefix
                .progress_file
                .to_string_lossy()
                .contains("progress.txt"),
            "progress file without prefix must contain 'progress.txt'"
        );
        // The negative: with_prefix must NOT end with plain progress.txt
        assert!(
            !with_prefix
                .progress_file
                .to_string_lossy()
                .ends_with("progress.txt"),
            "With prefix 'P1', file MUST NOT be plain 'progress.txt'"
        );
    }
}
