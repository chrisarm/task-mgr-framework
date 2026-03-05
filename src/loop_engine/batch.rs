//! Batch mode: run multiple PRDs in sequence.
//!
//! Expands a glob pattern to find PRD JSON files, derives prompt files,
//! validates all exist, then runs each sequentially via `run_loop()`.
//! Checks `.stop` signal between PRD executions.

use std::path::{Path, PathBuf};

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::config::LoopConfig;
use crate::loop_engine::engine::{self, LoopRunConfig};
use crate::loop_engine::signals;
use crate::loop_engine::worktree;

/// Result of a batch run.
#[derive(Debug)]
pub struct BatchResult {
    /// Number of PRDs that completed successfully (exit code 0).
    pub succeeded: usize,
    /// Number of PRDs that failed (exit code != 0).
    pub failed: usize,
    /// Number of PRDs skipped (due to .stop signal).
    pub skipped: usize,
    /// Per-PRD results in execution order.
    pub results: Vec<PrdRunResult>,
}

/// Result of running a single PRD within a batch.
#[derive(Debug)]
pub struct PrdRunResult {
    /// Path to the PRD JSON file.
    pub prd_file: PathBuf,
    /// Exit code from run_loop (0 = success).
    pub exit_code: i32,
    /// Whether this PRD was skipped.
    pub skipped: bool,
}

/// Expand a glob pattern into sorted PRD file paths.
///
/// Uses `globwalk` for expansion with natural sort (alphabetical).
/// Returns an error if no files match the pattern.
fn expand_glob(pattern: &str) -> TaskMgrResult<Vec<PathBuf>> {
    let walker = globwalk::glob(pattern).map_err(|e| {
        TaskMgrError::io_error(
            pattern,
            "expanding glob pattern",
            std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()),
        )
    })?;

    let mut files: Vec<PathBuf> = walker
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.into_path())
        .collect();

    if files.is_empty() {
        return Err(TaskMgrError::invalid_state(
            "batch",
            "glob pattern",
            "at least one matching file",
            format!("no files matched pattern '{}'", pattern),
        ));
    }

    // Natural sort: alphabetical (which handles numeric prefixes naturally
    // when files are named consistently, e.g., task-01.json, task-02.json)
    files.sort();

    Ok(files)
}

/// Derive prompt file path from PRD file path.
///
/// Strips `.json` extension and appends `-prompt.md`.
/// e.g., `.task-mgr/tasks/my-prd.json` → `.task-mgr/tasks/my-prd-prompt.md`
fn derive_prompt_file(prd_file: &Path) -> PathBuf {
    let stem = prd_file
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let parent = prd_file.parent().unwrap_or(Path::new("."));
    parent.join(format!("{}-prompt.md", stem))
}

/// Validate that all prompt files exist for the given PRD files.
///
/// Returns the list of (prd_file, prompt_file) pairs if all valid,
/// or an error listing which prompt files are missing.
fn validate_prompt_files(prd_files: &[PathBuf]) -> TaskMgrResult<Vec<(PathBuf, PathBuf)>> {
    let mut pairs = Vec::with_capacity(prd_files.len());
    let mut missing: Vec<String> = Vec::new();

    for prd_file in prd_files {
        let prompt_file = derive_prompt_file(prd_file);
        if !prompt_file.exists() {
            missing.push(format!(
                "  {} (for {})",
                prompt_file.display(),
                prd_file.display()
            ));
        }
        pairs.push((prd_file.clone(), prompt_file));
    }

    if !missing.is_empty() {
        return Err(TaskMgrError::invalid_state(
            "batch",
            "prompt files",
            "all prompt files present",
            format!("missing prompt files:\n{}", missing.join("\n")),
        ));
    }

    Ok(pairs)
}

/// Offer to clean up a worktree after a PRD run in batch mode.
///
/// Policy:
/// - `keep_worktrees = true` → never remove
/// - failed PRD (exit_code != 0) → keep regardless of flags (preserve for debugging)
/// - `cleanup_worktree = true` → auto-remove (explicit opt-in)
/// - `yes = true` without `cleanup_worktree` → keep (matches engine behavior)
/// - `yes = false` (interactive) → prompt user
/// - Cleanup failure warns but does not affect batch result
fn cleanup_worktree_after_prd(
    project_root: &Path,
    wt_path: &Path,
    exit_code: i32,
    yes: bool,
    keep_worktrees: bool,
    cleanup_worktree: bool,
) {
    if keep_worktrees {
        return;
    }

    if exit_code != 0 {
        // Keep worktrees from failed runs for debugging
        eprintln!("Keeping worktree (PRD failed): {}", wt_path.display());
        return;
    }

    let should_remove = if cleanup_worktree {
        // --cleanup-worktree flag: always attempt removal
        true
    } else if yes {
        // --yes without --cleanup-worktree: keep worktree (matches engine behavior)
        false
    } else {
        // Interactive: prompt user
        eprint!("Remove worktree '{}'? [y/N] ", wt_path.display());
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
        } else {
            false
        }
    };

    if should_remove {
        match worktree::remove_worktree(project_root, wt_path) {
            Ok(true) => eprintln!("Removed worktree: {}", wt_path.display()),
            Ok(false) => eprintln!(
                "Warning: worktree has uncommitted changes, kept: {}",
                wt_path.display()
            ),
            Err(e) => eprintln!(
                "Warning: failed to remove worktree '{}': {}",
                wt_path.display(),
                e
            ),
        }
    }
}

/// Run multiple PRDs in sequence.
///
/// 1. Expand glob pattern with natural sort
/// 2. Derive prompt files from PRD names
/// 3. Validate ALL prompt files exist before starting
/// 4. Run each PRD sequentially via `run_loop()`
/// 5. Check `.stop` signal between PRD executions
/// 6. Return summary of results
///
/// # Arguments
///
/// * `pattern` - Glob pattern to match PRD JSON files
/// * `max_iterations` - Optional max iterations per PRD (0 = auto)
/// * `yes` - Auto-confirm all prompts
/// * `dir` - Database directory (--dir flag)
/// * `project_root` - Git repository root for git operations and path resolution
/// * `verbose` - Verbose output
/// * `keep_worktrees` - Never remove worktrees after PRD completion
pub async fn run_batch(
    pattern: &str,
    max_iterations: Option<usize>,
    yes: bool,
    dir: &Path,
    project_root: &Path,
    verbose: bool,
    keep_worktrees: bool,
) -> BatchResult {
    // Step 1: Expand glob
    let prd_files = match expand_glob(pattern) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("Error: {}", e);
            return BatchResult {
                succeeded: 0,
                failed: 1,
                skipped: 0,
                results: vec![],
            };
        }
    };

    eprintln!(
        "Batch mode: found {} PRD file(s) matching '{}'",
        prd_files.len(),
        pattern
    );

    // Step 2: Validate all prompt files exist
    let pairs = match validate_prompt_files(&prd_files) {
        Ok(pairs) => pairs,
        Err(e) => {
            eprintln!("Error: {}", e);
            return BatchResult {
                succeeded: 0,
                failed: 1,
                skipped: 0,
                results: vec![],
            };
        }
    };

    // Step 3: Resolve tasks dir for .stop signal checking
    let tasks_dir = dir.join("tasks");

    // Step 4: Run each PRD sequentially
    let mut results = Vec::with_capacity(pairs.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;

    for (i, (prd_file, prompt_file)) in pairs.iter().enumerate() {
        // Check .stop signal between PRDs
        if i > 0 && signals::check_stop_signal(&tasks_dir, None) {
            eprintln!("Stop signal detected, skipping remaining PRDs");
            let remaining = pairs.len() - i;
            for (remaining_prd, _) in &pairs[i..] {
                results.push(PrdRunResult {
                    prd_file: remaining_prd.clone(),
                    exit_code: 0,
                    skipped: true,
                });
            }
            skipped += remaining;
            break;
        }

        eprintln!(
            "\n--- Batch [{}/{}]: {} ---",
            i + 1,
            pairs.len(),
            prd_file.display()
        );

        let mut config = LoopConfig::from_env();
        config.yes_mode = yes;
        config.verbose = verbose;
        if let Some(max_iter) = max_iterations {
            config.max_iterations = max_iter;
        }

        let run_config = LoopRunConfig {
            db_dir: dir.to_path_buf(),
            source_root: project_root.to_path_buf(),
            working_root: project_root.to_path_buf(), // May be updated by run_loop if using worktrees
            prd_file: prd_file.clone(),
            prompt_file: Some(prompt_file.clone()),
            config,
            external_repo: None, // Batch mode reads from PRD metadata
        };

        let should_cleanup_worktree = run_config.config.cleanup_worktree;
        let loop_result = engine::run_loop(run_config).await;
        let exit_code = loop_result.exit_code;
        let worktree_path = loop_result.worktree_path.clone();

        results.push(PrdRunResult {
            prd_file: prd_file.clone(),
            exit_code,
            skipped: false,
        });

        if exit_code == 0 {
            succeeded += 1;
        } else {
            failed += 1;
        }

        // Worktree cleanup after each PRD
        if let Some(ref wt_path) = worktree_path {
            cleanup_worktree_after_prd(
                project_root,
                wt_path,
                exit_code,
                yes,
                keep_worktrees,
                should_cleanup_worktree,
            );
        }
    }

    // Step 5: Print batch summary
    eprintln!("\n=== Batch Summary ===");
    eprintln!(
        "{} succeeded, {} failed, {} skipped (of {} total)",
        succeeded,
        failed,
        skipped,
        pairs.len()
    );

    for result in &results {
        let status = if result.skipped {
            "SKIPPED"
        } else if result.exit_code == 0 {
            "OK"
        } else {
            "FAILED"
        };
        eprintln!(
            "  [{}] {} (exit: {})",
            status,
            result.prd_file.display(),
            result.exit_code
        );
    }

    BatchResult {
        succeeded,
        failed,
        skipped,
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::STOP_FILE;
    use std::fs;
    use tempfile::TempDir;

    // --- derive_prompt_file tests ---

    #[test]
    fn test_derive_prompt_file_basic() {
        let prd = PathBuf::from(".task-mgr/tasks/my-prd.json");
        let prompt = derive_prompt_file(&prd);
        assert_eq!(prompt, PathBuf::from(".task-mgr/tasks/my-prd-prompt.md"));
    }

    #[test]
    fn test_derive_prompt_file_nested_path() {
        let prd = PathBuf::from("/home/user/project/.task-mgr/tasks/phase-1.json");
        let prompt = derive_prompt_file(&prd);
        assert_eq!(
            prompt,
            PathBuf::from("/home/user/project/.task-mgr/tasks/phase-1-prompt.md")
        );
    }

    #[test]
    fn test_derive_prompt_file_no_extension() {
        let prd = PathBuf::from(".task-mgr/tasks/my-prd");
        let prompt = derive_prompt_file(&prd);
        assert_eq!(prompt, PathBuf::from(".task-mgr/tasks/my-prd-prompt.md"));
    }

    #[test]
    fn test_derive_prompt_file_double_extension() {
        // file_stem() returns "my-prd.test" for "my-prd.test.json"
        let prd = PathBuf::from(".task-mgr/tasks/my-prd.test.json");
        let prompt = derive_prompt_file(&prd);
        assert_eq!(prompt, PathBuf::from(".task-mgr/tasks/my-prd.test-prompt.md"));
    }

    #[test]
    fn test_derive_prompt_file_current_dir() {
        let prd = PathBuf::from("prd.json");
        let prompt = derive_prompt_file(&prd);
        // parent() of "prd.json" returns "" (empty), which Path joins as just the filename
        assert_eq!(prompt, PathBuf::from("prd-prompt.md"));
    }

    // --- validate_prompt_files tests ---

    #[test]
    fn test_validate_prompt_files_all_exist() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let prd_path = temp_dir.path().join("test.json");
        let prompt_path = temp_dir.path().join("test-prompt.md");
        fs::write(&prd_path, "{}").expect("write prd");
        fs::write(&prompt_path, "# Prompt").expect("write prompt");

        let result = validate_prompt_files(&[prd_path.clone()]);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, prd_path);
        assert_eq!(pairs[0].1, prompt_path);
    }

    #[test]
    fn test_validate_prompt_files_missing_prompt() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let prd_path = temp_dir.path().join("test.json");
        fs::write(&prd_path, "{}").expect("write prd");
        // Don't create prompt file

        let result = validate_prompt_files(&[prd_path]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing prompt files"), "Error: {}", err);
    }

    #[test]
    fn test_validate_prompt_files_multiple_some_missing() {
        let temp_dir = TempDir::new().expect("create temp dir");

        let prd1 = temp_dir.path().join("a.json");
        let prompt1 = temp_dir.path().join("a-prompt.md");
        fs::write(&prd1, "{}").expect("write prd1");
        fs::write(&prompt1, "# A").expect("write prompt1");

        let prd2 = temp_dir.path().join("b.json");
        fs::write(&prd2, "{}").expect("write prd2");
        // Don't create b-prompt.md

        let result = validate_prompt_files(&[prd1, prd2]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("b-prompt.md"),
            "Error should mention missing file: {}",
            err
        );
    }

    #[test]
    fn test_validate_prompt_files_empty_list() {
        let result = validate_prompt_files(&[]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    // --- expand_glob tests ---

    #[test]
    fn test_expand_glob_matches_json_files() {
        let temp_dir = TempDir::new().expect("create temp dir");
        fs::write(temp_dir.path().join("a.json"), "{}").expect("write a");
        fs::write(temp_dir.path().join("b.json"), "{}").expect("write b");
        fs::write(temp_dir.path().join("c.txt"), "").expect("write c");

        let pattern = format!("{}/*.json", temp_dir.path().display());
        let result = expand_glob(&pattern);
        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 2);
        // Should be sorted
        assert!(
            files[0].file_name().unwrap().to_string_lossy()
                < files[1].file_name().unwrap().to_string_lossy(),
            "Files should be sorted: {:?}",
            files
        );
    }

    #[test]
    fn test_expand_glob_no_matches() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let pattern = format!("{}/*.nonexistent", temp_dir.path().display());
        let result = expand_glob(&pattern);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no files matched"), "Error: {}", err);
    }

    #[test]
    fn test_expand_glob_natural_sort_order() {
        let temp_dir = TempDir::new().expect("create temp dir");
        // Create files that test alphabetical sort
        fs::write(temp_dir.path().join("task-02.json"), "{}").expect("write");
        fs::write(temp_dir.path().join("task-01.json"), "{}").expect("write");
        fs::write(temp_dir.path().join("task-10.json"), "{}").expect("write");

        let pattern = format!("{}/*.json", temp_dir.path().display());
        let files = expand_glob(&pattern).expect("glob should succeed");
        assert_eq!(files.len(), 3);

        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["task-01.json", "task-02.json", "task-10.json"]);
    }

    #[test]
    fn test_expand_glob_single_match() {
        let temp_dir = TempDir::new().expect("create temp dir");
        fs::write(temp_dir.path().join("only.json"), "{}").expect("write");

        let pattern = format!("{}/*.json", temp_dir.path().display());
        let files = expand_glob(&pattern).expect("glob should succeed");
        assert_eq!(files.len(), 1);
    }

    // --- BatchResult / PrdRunResult tests ---

    #[test]
    fn test_batch_result_fields() {
        let result = BatchResult {
            succeeded: 2,
            failed: 1,
            skipped: 0,
            results: vec![
                PrdRunResult {
                    prd_file: PathBuf::from("a.json"),
                    exit_code: 0,
                    skipped: false,
                },
                PrdRunResult {
                    prd_file: PathBuf::from("b.json"),
                    exit_code: 0,
                    skipped: false,
                },
                PrdRunResult {
                    prd_file: PathBuf::from("c.json"),
                    exit_code: 1,
                    skipped: false,
                },
            ],
        };
        assert_eq!(result.succeeded, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.results.len(), 3);
    }

    #[test]
    fn test_prd_run_result_skipped() {
        let result = PrdRunResult {
            prd_file: PathBuf::from("skipped.json"),
            exit_code: 0,
            skipped: true,
        };
        assert!(result.skipped);
        assert_eq!(result.exit_code, 0);
    }

    // --- Stop signal between PRDs tests ---

    #[test]
    fn test_stop_signal_detected_between_prds() {
        // Verify that check_stop_signal works with .stop file
        let temp_dir = TempDir::new().expect("create temp dir");
        assert!(!signals::check_stop_signal(temp_dir.path(), None));

        fs::write(temp_dir.path().join(STOP_FILE), "").expect("create stop");
        assert!(signals::check_stop_signal(temp_dir.path(), None));
    }

    // --- cleanup_worktree_after_prd tests ---

    fn init_test_repo_for_batch() -> (TempDir, std::path::PathBuf) {
        crate::loop_engine::test_utils::init_test_repo()
    }

    #[test]
    fn test_cleanup_keep_worktrees_flag_preserves_worktree() {
        // When keep_worktrees=true, cleanup should return immediately without removing anything.
        let tmp = TempDir::new().expect("create temp dir");
        let dummy_path = tmp.path().join("worktree");
        fs::create_dir_all(&dummy_path).expect("create dummy dir");

        // Pass keep_worktrees=true — function should return without touching path
        cleanup_worktree_after_prd(tmp.path(), &dummy_path, 0, true, true, false);

        assert!(
            dummy_path.exists(),
            "worktree dir should still exist when keep_worktrees=true"
        );
    }

    #[test]
    fn test_cleanup_failed_prd_keeps_worktree() {
        // When exit_code != 0, cleanup should keep the worktree regardless of yes/keep flags.
        let tmp = TempDir::new().expect("create temp dir");
        let dummy_path = tmp.path().join("worktree");
        fs::create_dir_all(&dummy_path).expect("create dummy dir");

        // exit_code=1 → keep worktree for debugging
        cleanup_worktree_after_prd(tmp.path(), &dummy_path, 1, true, false, true);

        assert!(
            dummy_path.exists(),
            "worktree dir should be kept when PRD failed (exit_code=1)"
        );
    }

    #[test]
    fn test_cleanup_success_auto_yes_removes_worktree() {
        // When exit_code=0 and yes=true, cleanup should attempt to remove the worktree.
        let (tmp, repo) = init_test_repo_for_batch();

        // Create a real worktree to remove
        let wt_path = tmp.path().join("cleanup-wt");
        std::process::Command::new("git")
            .args(["branch", "feat/cleanup-test"])
            .current_dir(&repo)
            .output()
            .expect("git branch");
        std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                wt_path.to_str().expect("valid path"),
                "feat/cleanup-test",
            ])
            .current_dir(&repo)
            .output()
            .expect("git worktree add");

        assert!(wt_path.exists(), "worktree must exist before cleanup");

        // exit_code=0, cleanup_worktree=true → should remove
        cleanup_worktree_after_prd(&repo, &wt_path, 0, true, false, true);

        assert!(
            !wt_path.exists(),
            "worktree dir should be removed on successful PRD with yes=true"
        );
    }
}
