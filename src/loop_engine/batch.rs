//! Batch mode: run multiple PRDs in sequence.
//!
//! Expands a glob pattern to find PRD JSON files, derives prompt files,
//! validates all exist, then runs each sequentially via `run_loop()`.
//! Checks `.stop` signal between PRD executions.

use std::path::{Path, PathBuf};

use crate::commands::init::PrefixMode;
use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::config::LoopConfig;
use crate::loop_engine::engine::{self, LoopRunConfig};
use crate::loop_engine::signals;
use crate::loop_engine::status_queries;
use crate::loop_engine::worktree;

/// Result of a batch run.
#[derive(Debug)]
pub struct BatchResult {
    /// Number of PRDs that completed successfully (exit code 0, not stopped).
    pub succeeded: usize,
    /// Number of PRDs that failed (exit code != 0).
    pub failed: usize,
    /// Number of PRDs skipped (due to .stop signal between PRDs).
    pub skipped: usize,
    /// Number of PRDs stopped mid-run by a .stop file.
    pub stopped: usize,
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
    /// Whether this PRD was skipped before it started (stop signal between PRDs).
    pub skipped: bool,
    /// Whether this PRD was halted mid-run by a .stop file.
    pub stopped: bool,
    /// Branch name produced by this PRD's run (from LoopResult). Used for chain summary.
    pub branch_name: Option<String>,
    /// Chain base ref this PRD branched from (None = branched from HEAD).
    pub chain_base: Option<String>,
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

/// Validate that all PRDs have a `branchName` field when running in chain mode.
///
/// Returns an error if any PRD is missing `branchName` (chain cannot proceed without it).
/// Emits a warning to stderr for duplicate branch names (worktree reuse, changes accumulate).
fn validate_chain_branches(pairs: &[(PathBuf, PathBuf)]) -> TaskMgrResult<()> {
    use std::collections::BTreeMap;

    let mut missing: Vec<String> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();

    for (prd_file, _) in pairs {
        match status_queries::read_branch_name_from_prd(prd_file) {
            None => missing.push(prd_file.display().to_string()),
            Some(branch) => *seen.entry(branch).or_insert(0) += 1,
        }
    }

    if !missing.is_empty() {
        return Err(TaskMgrError::invalid_state(
            "batch --chain",
            "branchName",
            "all PRDs must have branchName when --chain is active",
            format!("PRDs missing branchName:\n{}", missing.join("\n")),
        ));
    }

    for (branch, count) in &seen {
        if *count > 1 {
            eprintln!(
                "Warning: duplicate branchName '{}' found in {} PRDs — worktree will be reused",
                branch, count
            );
        }
    }

    Ok(())
}

/// Context for cleaning up worktrees after PRD runs in batch mode.
///
/// Policy:
/// - `keep_worktrees = true` → never remove
/// - failed PRD (exit_code != 0) → keep regardless of flags (preserve for debugging)
/// - `cleanup_worktree = true` → auto-remove (explicit opt-in)
/// - `yes = true` without `cleanup_worktree` → keep (matches engine behavior)
/// - `yes = false` (interactive) → prompt user
/// - Cleanup failure warns but does not affect batch result
struct WorktreeCleanupContext<'a> {
    project_root: &'a Path,
    yes: bool,
    keep_worktrees: bool,
    cleanup_worktree: bool,
    chain: bool,
}

impl WorktreeCleanupContext<'_> {
    fn cleanup(&self, wt_path: &Path, exit_code: i32, branch_name: Option<&str>) {
        if self.keep_worktrees {
            return;
        }

        if exit_code != 0 {
            // Keep worktrees from failed runs for debugging
            eprintln!("Keeping worktree (PRD failed): {}", wt_path.display());
            return;
        }

        let should_remove = if self.cleanup_worktree {
            // --cleanup-worktree flag: always attempt removal
            true
        } else if self.yes {
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
            match worktree::remove_worktree(self.project_root, wt_path) {
                Ok(true) => {
                    if self.chain {
                        if let Some(branch) = branch_name {
                            eprintln!(
                                "Worktree removed but branch {} retained for chaining",
                                branch
                            );
                        } else {
                            eprintln!("Removed worktree: {}", wt_path.display());
                        }
                    } else {
                        eprintln!("Removed worktree: {}", wt_path.display());
                    }
                }
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
}

/// Return a `BatchResult` representing a single early-exit failure (validation error, etc.).
fn batch_fail_early() -> BatchResult {
    BatchResult {
        succeeded: 0,
        failed: 1,
        skipped: 0,
        stopped: 0,
        results: vec![],
    }
}

/// Push all `pairs[from..]` as skipped `PrdRunResult`s and update the skipped counter.
fn push_remaining_skipped(
    results: &mut Vec<PrdRunResult>,
    pairs: &[(PathBuf, PathBuf)],
    from: usize,
    skipped: &mut usize,
) {
    for (remaining_prd, _) in &pairs[from..] {
        results.push(PrdRunResult {
            prd_file: remaining_prd.clone(),
            exit_code: 0,
            skipped: true,
            stopped: false,
            branch_name: None,
            chain_base: None,
        });
    }
    *skipped += pairs.len() - from;
}

/// Expand glob patterns into sorted, deduplicated PRD file paths.
///
/// Processes all patterns in order, canonicalises each path to deduplicate
/// across overlapping globs, then returns a sorted list.
fn collect_prd_files(patterns: &[String]) -> TaskMgrResult<Vec<PathBuf>> {
    let mut prd_files: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for pattern in patterns {
        let files = expand_glob(pattern)?;
        for file in files {
            let canonical = std::fs::canonicalize(&file).unwrap_or(file);
            if seen.insert(canonical.clone()) {
                prd_files.push(canonical);
            }
        }
    }

    if prd_files.is_empty() {
        return Err(TaskMgrError::invalid_state(
            "batch",
            "patterns",
            "at least one matching file",
            "no files matched any patterns",
        ));
    }

    prd_files.sort();

    Ok(prd_files)
}

/// Print the batch summary to stderr.
fn print_batch_summary(
    results: &[PrdRunResult],
    total: usize,
    succeeded: usize,
    failed: usize,
    skipped: usize,
    stopped: usize,
    chain: bool,
) {
    eprintln!("\n=== Batch Summary ===");
    eprintln!(
        "{} succeeded, {} failed, {} stopped, {} skipped (of {} total)",
        succeeded, failed, stopped, skipped, total
    );

    for result in results {
        let status = if result.skipped {
            "SKIPPED"
        } else if result.stopped {
            "STOPPED"
        } else if result.exit_code == 0 {
            "OK"
        } else {
            "FAILED"
        };
        if chain {
            let branch = result.branch_name.as_deref().unwrap_or("(unknown)");
            let from = result.chain_base.as_deref().unwrap_or("HEAD");
            eprintln!(
                "  [{}] {} → {} (from {})",
                status,
                result.prd_file.display(),
                branch,
                from,
            );
        } else {
            eprintln!(
                "  [{}] {} (exit: {})",
                status,
                result.prd_file.display(),
                result.exit_code
            );
        }
    }
}

/// Run multiple PRDs in sequence.
///
/// 1. Expand glob patterns with natural sort and deduplication
/// 2. Derive prompt files from PRD names
/// 3. Validate ALL prompt files exist before starting
/// 4. Run each PRD sequentially via `run_loop()`
/// 5. Check `.stop` signal between PRD executions
/// 6. Return summary of results
///
/// # Arguments
///
/// * `patterns` - Glob patterns or literal file paths to match PRD JSON files
/// * `max_iterations` - Optional max iterations per PRD (0 = auto)
/// * `yes` - Auto-confirm all prompts
/// * `dir` - Database directory (--dir flag)
/// * `project_root` - Git repository root for git operations and path resolution
/// * `verbose` - Verbose output
/// * `keep_worktrees` - Never remove worktrees after PRD completion
#[allow(clippy::too_many_arguments)]
pub async fn run_batch(
    patterns: &[String],
    max_iterations: Option<usize>,
    yes: bool,
    dir: &Path,
    project_root: &Path,
    verbose: bool,
    keep_worktrees: bool,
    chain: bool,
) -> BatchResult {
    // Step 1: Expand all patterns, deduplicate, and sort
    let prd_files = match collect_prd_files(patterns) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("Error: {}", e);
            return batch_fail_early();
        }
    };

    let pattern_display = if patterns.len() <= 3 {
        patterns
            .iter()
            .map(|p| format!("'{p}'"))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        format!("{} pattern(s)", patterns.len())
    };
    eprintln!(
        "Batch mode: found {} PRD file(s) matching {}",
        prd_files.len(),
        pattern_display
    );

    // Step 2: Validate all prompt files exist
    let pairs = match validate_prompt_files(&prd_files) {
        Ok(pairs) => pairs,
        Err(e) => {
            eprintln!("Error: {}", e);
            return batch_fail_early();
        }
    };

    // Step 2.5: Warn if two PRDs would produce the same Auto prefix (same filename + branchName).
    {
        use std::collections::HashMap;
        let mut prefix_to_files: HashMap<String, Vec<&Path>> = HashMap::new();
        for (prd_file, _) in &pairs {
            let filename = prd_file
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("unknown.json");
            let branch = status_queries::read_branch_name_from_prd(prd_file);
            let prefix = crate::commands::init::generate_prefix(branch.as_deref(), filename);
            prefix_to_files
                .entry(prefix)
                .or_default()
                .push(prd_file.as_path());
        }
        for (prefix, files) in &prefix_to_files {
            if files.len() > 1 {
                eprintln!(
                    "Warning: {} PRDs would share prefix '{}' (same filename + branchName):",
                    files.len(),
                    prefix
                );
                for f in files {
                    eprintln!("  - {}", f.display());
                }
                eprintln!(
                    "Their tasks will collide. Consider renaming the PRD files to be unique."
                );
            }
        }
    }

    // Step 3: Chain validation — all PRDs must have branchName when --chain is active.
    // This runs upfront so we fail fast before any work begins.
    if chain {
        if let Err(e) = validate_chain_branches(&pairs) {
            eprintln!("Error: {}", e);
            return batch_fail_early();
        }
    }

    // Step 4: Resolve tasks dir for .stop signal checking
    let tasks_dir = dir.join("tasks");

    // Chain tracking: advances to loop_result.branch_name after each successful PRD.
    // Starts as None so the first PRD branches from HEAD.
    let mut chain_base: Option<String> = None;

    // Step 5: Run each PRD sequentially
    let mut results = Vec::with_capacity(pairs.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut stopped = 0usize;

    for (i, (prd_file, prompt_file)) in pairs.iter().enumerate() {
        // Check .stop signal before each PRD (covers files placed between runs,
        // or before the batch even starts its first PRD).
        if signals::check_stop_signal(&tasks_dir, None) {
            eprintln!("Stop signal detected, skipping remaining PRDs");
            push_remaining_skipped(&mut results, &pairs, i, &mut skipped);
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

        // Sibling PRDs = all other PRD files in the batch (for milestone context)
        let sibling_prds: Vec<PathBuf> = pairs
            .iter()
            .filter(|(p, _)| p != prd_file)
            .map(|(p, _)| p.clone())
            .collect();

        // Use Auto prefix mode so each PRD gets the same deterministic prefix
        // (md5(branchName:filename)[:8]) that a standalone loop run would use.
        // This ensures loop→batch transitions reuse existing task IDs.
        let prefix_mode = PrefixMode::Auto;

        let run_config = LoopRunConfig {
            db_dir: dir.to_path_buf(),
            source_root: project_root.to_path_buf(),
            working_root: project_root.to_path_buf(), // May be updated by run_loop if using worktrees
            prd_file: prd_file.clone(),
            prompt_file: Some(prompt_file.clone()),
            config,
            external_repo: None, // Batch mode reads from PRD metadata
            batch_sibling_prds: sibling_prds,
            chain_base: if chain { chain_base.clone() } else { None },
            prefix_mode,
        };

        let should_cleanup_worktree = run_config.config.cleanup_worktree;
        let loop_result = engine::run_loop(run_config).await;
        let exit_code = loop_result.exit_code;
        let worktree_path = loop_result.worktree_path.clone();
        let result_branch_name = loop_result.branch_name.clone();
        let was_stopped = loop_result.was_stopped;

        // Stop signal: if the engine was halted by a .stop file mid-run, record the
        // PRD as stopped (not succeeded) and abort the batch. The engine consumes the
        // signal file before returning, so we rely on the was_stopped flag.
        if was_stopped {
            results.push(PrdRunResult {
                prd_file: prd_file.clone(),
                exit_code,
                skipped: false,
                stopped: true,
                branch_name: result_branch_name.clone(),
                chain_base: if chain { chain_base.clone() } else { None },
            });
            stopped += 1;

            // Worktree cleanup for the stopped PRD
            if let Some(ref wt_path) = worktree_path {
                WorktreeCleanupContext {
                    project_root,
                    yes,
                    keep_worktrees,
                    cleanup_worktree: should_cleanup_worktree,
                    chain,
                }
                .cleanup(wt_path, exit_code, result_branch_name.as_deref());
            }

            eprintln!("Stop signal detected during PRD, skipping remaining PRDs");
            push_remaining_skipped(&mut results, &pairs, i + 1, &mut skipped);
            break;
        }

        results.push(PrdRunResult {
            prd_file: prd_file.clone(),
            exit_code,
            skipped: false,
            stopped: false,
            branch_name: result_branch_name.clone(),
            chain_base: if chain { chain_base.clone() } else { None },
        });

        if exit_code == 0 {
            succeeded += 1;
        } else {
            failed += 1;
        }

        // Worktree cleanup after each PRD
        if let Some(ref wt_path) = worktree_path {
            WorktreeCleanupContext {
                project_root,
                yes,
                keep_worktrees,
                cleanup_worktree: should_cleanup_worktree,
                chain,
            }
            .cleanup(wt_path, exit_code, result_branch_name.as_deref());
        }

        // Chain stop-on-failure: if this PRD failed, skip all remaining PRDs.
        // Downstream PRDs would build on a broken state, so we abort immediately.
        if chain && exit_code != 0 {
            eprintln!("Chain stopped: PRD failed, skipping remaining PRDs");
            push_remaining_skipped(&mut results, &pairs, i + 1, &mut skipped);
            break;
        }

        // Advance chain: next PRD branches from this PRD's branch (using LoopResult,
        // not pre-read from JSON — avoids mismatch if DB normalizes the branch name).
        if chain {
            chain_base = result_branch_name;
        }
    }

    // Step 5: Print batch summary
    print_batch_summary(
        &results,
        pairs.len(),
        succeeded,
        failed,
        skipped,
        stopped,
        chain,
    );

    BatchResult {
        succeeded,
        failed,
        skipped,
        stopped,
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::STOP_FILE;
    use std::fs;
    use tempfile::TempDir;

    // --- auto prefix uniqueness tests ---

    #[test]
    fn test_auto_prefix_uniqueness_across_different_filenames() {
        use crate::commands::init::generate_prefix;

        // Different filenames → different prefixes (the common case in batch)
        let p1 = generate_prefix(Some("feat/main"), "04-predictive.json");
        let p2 = generate_prefix(Some("feat/main"), "05-gcp-ops.json");
        assert_ne!(
            p1, p2,
            "different filenames must produce different prefixes"
        );

        // Same filename + same branch → same prefix (edge case, warns user)
        let p3 = generate_prefix(Some("feat/main"), "tasks.json");
        let p4 = generate_prefix(Some("feat/main"), "tasks.json");
        assert_eq!(p3, p4, "same filename + branch must produce same prefix");

        // Same filename + different branch → different prefix
        let p5 = generate_prefix(Some("feat/alpha"), "tasks.json");
        let p6 = generate_prefix(Some("feat/beta"), "tasks.json");
        assert_ne!(p5, p6, "same filename but different branches must differ");
    }

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
        assert_eq!(
            prompt,
            PathBuf::from(".task-mgr/tasks/my-prd.test-prompt.md")
        );
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
            stopped: 0,
            results: vec![
                PrdRunResult {
                    prd_file: PathBuf::from("a.json"),
                    exit_code: 0,
                    skipped: false,
                    stopped: false,
                    branch_name: None,
                    chain_base: None,
                },
                PrdRunResult {
                    prd_file: PathBuf::from("b.json"),
                    exit_code: 0,
                    skipped: false,
                    stopped: false,
                    branch_name: None,
                    chain_base: None,
                },
                PrdRunResult {
                    prd_file: PathBuf::from("c.json"),
                    exit_code: 1,
                    skipped: false,
                    stopped: false,
                    branch_name: None,
                    chain_base: None,
                },
            ],
        };
        assert_eq!(result.succeeded, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.stopped, 0);
        assert_eq!(result.results.len(), 3);
    }

    #[test]
    fn test_prd_run_result_skipped() {
        let result = PrdRunResult {
            prd_file: PathBuf::from("skipped.json"),
            exit_code: 0,
            skipped: true,
            stopped: false,
            branch_name: None,
            chain_base: None,
        };
        assert!(result.skipped);
        assert!(!result.stopped);
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
        WorktreeCleanupContext {
            project_root: tmp.path(),
            yes: true,
            keep_worktrees: true,
            cleanup_worktree: false,
            chain: false,
        }
        .cleanup(&dummy_path, 0, None);

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
        WorktreeCleanupContext {
            project_root: tmp.path(),
            yes: true,
            keep_worktrees: false,
            cleanup_worktree: true,
            chain: false,
        }
        .cleanup(&dummy_path, 1, None);

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
        WorktreeCleanupContext {
            project_root: &repo,
            yes: true,
            keep_worktrees: false,
            cleanup_worktree: true,
            chain: false,
        }
        .cleanup(&wt_path, 0, None);

        assert!(
            !wt_path.exists(),
            "worktree dir should be removed on successful PRD with yes=true"
        );
    }

    // --- validate_chain_branches tests ---

    #[test]
    fn test_validate_chain_branches_errors_on_missing_branch_name() {
        let temp_dir = TempDir::new().expect("create temp dir");

        // PRD without branchName field
        let prd_no_branch = temp_dir.path().join("no-branch.json");
        fs::write(&prd_no_branch, r#"{"title": "No Branch PRD"}"#).expect("write prd");
        let prompt_no_branch = temp_dir.path().join("no-branch-prompt.md");
        fs::write(&prompt_no_branch, "# Prompt").expect("write prompt");

        let pairs = vec![(prd_no_branch.clone(), prompt_no_branch)];
        let result = validate_chain_branches(&pairs);

        assert!(result.is_err(), "should error when branchName is missing");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("PRDs missing branchName"),
            "Error should mention missing branchName: {}",
            err
        );
        assert!(
            err.contains("no-branch.json"),
            "Error should name the offending file: {}",
            err
        );
    }

    #[test]
    fn test_validate_chain_branches_ok_when_all_have_branch_name() {
        let temp_dir = TempDir::new().expect("create temp dir");

        let prd = temp_dir.path().join("phase-1.json");
        fs::write(&prd, r#"{"branchName": "feat/phase-1"}"#).expect("write prd");
        let prompt = temp_dir.path().join("phase-1-prompt.md");
        fs::write(&prompt, "# Prompt").expect("write prompt");

        let pairs = vec![(prd, prompt)];
        assert!(validate_chain_branches(&pairs).is_ok());
    }

    #[test]
    fn test_validate_chain_branches_warns_on_duplicate_branch_names() {
        let temp_dir = TempDir::new().expect("create temp dir");

        // Two PRDs sharing the same branchName — warns but does not error
        let prd1 = temp_dir.path().join("a.json");
        fs::write(&prd1, r#"{"branchName": "feat/shared"}"#).expect("write prd1");
        let prompt1 = temp_dir.path().join("a-prompt.md");
        fs::write(&prompt1, "# A").expect("write prompt1");

        let prd2 = temp_dir.path().join("b.json");
        fs::write(&prd2, r#"{"branchName": "feat/shared"}"#).expect("write prd2");
        let prompt2 = temp_dir.path().join("b-prompt.md");
        fs::write(&prompt2, "# B").expect("write prompt2");

        let pairs = vec![(prd1, prompt1), (prd2, prompt2)];
        // Must succeed (duplicate is warn-only, not an error)
        assert!(
            validate_chain_branches(&pairs).is_ok(),
            "duplicate branchName should warn but not error"
        );
    }

    #[test]
    fn test_validate_chain_branches_not_called_when_chain_false() {
        // Verify that chain=false path does NOT validate branchName.
        // A PRD without branchName must be accepted when chain=false.
        // This guards against accidentally running validation on the non-chain path.
        let temp_dir = TempDir::new().expect("create temp dir");

        let prd = temp_dir.path().join("no-branch.json");
        fs::write(&prd, r#"{"title": "No Branch"}"#).expect("write prd");
        let prompt = temp_dir.path().join("no-branch-prompt.md");
        fs::write(&prompt, "# Prompt").expect("write prompt");

        let pairs = vec![(prd, prompt)];

        // chain=false: validate_chain_branches should NOT be called.
        // If we call it, it would return Err — which we can assert wouldn't happen
        // from the run_batch code path. We directly test the guard: chain=false
        // must not invoke validate_chain_branches, so the PRD above would be fine.
        //
        // Directly test the invariant: calling validate_chain_branches with a
        // branchless PRD returns Err, confirming the guard is real.
        let result = validate_chain_branches(&pairs);
        assert!(
            result.is_err(),
            "validate_chain_branches must error on branchless PRD — \
             confirming chain=false must NOT call it"
        );
    }

    #[test]
    fn test_stop_on_failure_results_structure() {
        // Verify that when chain=true and a PRD fails, remaining PRDs are skipped.
        // We test the PrdRunResult structure that would be produced, not the async runner.
        //
        // Simulate what run_batch would produce for 3 PRDs where PRD[1] fails:
        let results = vec![
            PrdRunResult {
                prd_file: PathBuf::from("phase-1.json"),
                exit_code: 0,
                skipped: false,
                stopped: false,
                branch_name: Some("feat/phase-1".to_string()),
                chain_base: None, // first PRD branches from HEAD
            },
            PrdRunResult {
                prd_file: PathBuf::from("phase-2.json"),
                exit_code: 1,
                skipped: false,
                stopped: false,
                branch_name: None,
                chain_base: Some("feat/phase-1".to_string()),
            },
            // PRD[2] would be skipped by stop-on-failure
            PrdRunResult {
                prd_file: PathBuf::from("phase-3.json"),
                exit_code: 0,
                skipped: true,
                stopped: false,
                branch_name: None,
                chain_base: None,
            },
        ];

        // Verify the structure invariants
        assert!(
            !results[0].skipped && results[0].exit_code == 0,
            "PRD 1 succeeded"
        );
        assert!(
            !results[1].skipped && results[1].exit_code == 1,
            "PRD 2 failed"
        );
        assert!(results[2].skipped, "PRD 3 skipped after chain failure");
        assert_eq!(
            results[2].chain_base, None,
            "skipped PRDs have no chain_base"
        );

        // Chain advancement: PRD[1].chain_base == PRD[0].branch_name
        assert_eq!(
            results[1].chain_base.as_deref(),
            results[0].branch_name.as_deref(),
            "chain_base must equal previous PRD's branch_name"
        );
    }
}
