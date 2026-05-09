//! Worktree lifecycle management for the loop engine.
//!
//! Provides functions to sanitize branch names, compute worktree paths,
//! detect worktree context, parse git worktree output, and create/remove
//! git worktrees with proper cleanup.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{TaskMgrError, TaskMgrResult};

use super::env::{get_current_branch, prompt_user_yn};

/// Silently ignore errors (best-effort cleanup).
fn cleanup_empty_dir(path: &Path) {
    if path.exists()
        && let Ok(mut entries) = std::fs::read_dir(path)
        && entries.next().is_none()
    {
        let _ = std::fs::remove_dir(path);
    }
}

/// Replace `/`, spaces, and other problematic characters with `-`.
fn sanitize_branch_name(branch_name: &str) -> String {
    branch_name
        .chars()
        .map(|c| match c {
            '/' | ' ' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect()
}

/// Return `{repo-parent}/{repo-name}-worktrees/{sanitized-branch-name}/`.
pub(crate) fn compute_worktree_path(project_root: &Path, branch_name: &str) -> PathBuf {
    let repo_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let parent = project_root.parent().unwrap_or(project_root);
    let worktrees_dir = parent.join(format!("{}-worktrees", repo_name));
    let sanitized = sanitize_branch_name(branch_name);

    worktrees_dir.join(sanitized)
}

fn is_inside_worktree(dir: &Path) -> TaskMgrResult<bool> {
    crate::git::is_inside_worktree_at(dir)
        .map_err(|e| TaskMgrError::io_error(dir.display().to_string(), "running git rev-parse", e))
}

/// Return a list of (worktree_path, branch_name) tuples.
pub(crate) fn parse_worktree_list(output: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // Save previous worktree if any
            if let Some(p) = current_path.take() {
                worktrees.push((p, current_branch.take()));
            }
            current_path = Some(PathBuf::from(path));
            current_branch = None;
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        }
    }

    // Don't forget the last one
    if let Some(p) = current_path {
        worktrees.push((p, current_branch));
    }

    worktrees
}

/// Return root directories of all git worktrees except `exclude_root`.
///
/// Returns an empty vec if git is unavailable or `git worktree list` fails.
/// Comparison is done on canonicalized paths to handle symlinks and trailing slashes.
pub(crate) fn list_other_roots(exclude_root: &Path) -> Vec<PathBuf> {
    let output = match Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(exclude_root)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let raw = String::from_utf8_lossy(&output.stdout);
    let canonical_exclude = exclude_root.canonicalize().ok();

    parse_worktree_list(&raw)
        .into_iter()
        .filter_map(|(path, _branch)| {
            let canonical = path.canonicalize().ok()?;
            if canonical_exclude.as_ref() == Some(&canonical) {
                None
            } else {
                Some(path)
            }
        })
        .collect()
}

/// Create a worktree at `{repo-parent}/{repo-name}-worktrees/{sanitized-branch}/`
/// if one doesn't already exist for this branch.
///
/// # Arguments
///
/// * `project_root` - Path to the main git repository
/// * `branch_name` - Target branch name
/// * `yes_mode` - If false, prompts user before creating worktree
/// * `start_point` - Optional git ref to branch from when creating a NEW branch.
///   Passed as `-- <start_point>` to prevent flag injection. Ignored if the branch
///   already exists.
///
/// # Returns
///
/// Path to the worktree directory (existing or newly created).
///
/// # Errors
///
/// Returns an error if:
/// - Git commands fail
/// - User declines to create worktree (interactive mode)
/// - Already inside a worktree for a different branch
pub fn ensure_worktree(
    project_root: &Path,
    branch_name: &str,
    yes_mode: bool,
    start_point: Option<&str>,
) -> TaskMgrResult<PathBuf> {
    // Check if we're already inside a worktree
    if is_inside_worktree(project_root)? {
        let current = get_current_branch(project_root)?;
        if current == branch_name {
            // Already in the correct worktree, use it as-is
            return Ok(project_root.to_path_buf());
        } else {
            return Err(TaskMgrError::InvalidState {
                resource_type: "Git worktree".to_string(),
                id: project_root.display().to_string(),
                expected: format!("worktree for branch '{}'", branch_name),
                actual: format!(
                    "already inside worktree for branch '{}'. \
                     Run from the main repository or the correct worktree.",
                    current
                ),
            });
        }
    }

    let worktree_path = compute_worktree_path(project_root, branch_name);

    // Check if worktree already exists via git worktree list
    let list_output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "running git worktree list",
                e,
            )
        })?;

    if list_output.status.success() {
        let list_str = String::from_utf8_lossy(&list_output.stdout);
        let worktrees = parse_worktree_list(&list_str);

        // Check if a worktree for this branch already exists
        for (path, branch) in &worktrees {
            if branch.as_deref() == Some(branch_name) {
                // Found existing worktree for this branch
                if path.exists() {
                    eprintln!(
                        "Using existing worktree for '{}' at {}",
                        branch_name,
                        path.display()
                    );
                    return Ok(path.clone());
                }
            }
        }

        // Check if our target path is already a worktree (but maybe for a different branch)
        if worktree_path.exists() && worktree_path.join(".git").exists() {
            // It's a worktree, check which branch
            let wt_branch = get_current_branch(&worktree_path)?;
            if wt_branch == branch_name {
                eprintln!(
                    "Using existing worktree for '{}' at {}",
                    branch_name,
                    worktree_path.display()
                );
                return Ok(worktree_path);
            } else {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "Git worktree".to_string(),
                    id: worktree_path.display().to_string(),
                    expected: format!("worktree for branch '{}'", branch_name),
                    actual: format!(
                        "worktree exists but is on branch '{}'. \
                         Remove it with: git worktree remove {}",
                        wt_branch,
                        worktree_path.display()
                    ),
                });
            }
        }
    }

    // Need to create the worktree
    if !yes_mode {
        eprintln!(
            "Creating git worktree for branch '{}' at {}",
            branch_name,
            worktree_path.display()
        );
        if !prompt_user_yn("Create worktree? [y/N] ")? {
            return Err(TaskMgrError::InvalidState {
                resource_type: "User confirmation".to_string(),
                id: "worktree creation".to_string(),
                expected: "user approved worktree creation".to_string(),
                actual: "user declined".to_string(),
            });
        }
    } else {
        eprintln!(
            "Creating worktree for '{}' at {}",
            branch_name,
            worktree_path.display()
        );
    }

    // Create parent directory for worktrees; track if we created it so we can
    // remove it on failure (avoids leaving orphan directories behind).
    let worktrees_parent = worktree_path.parent().unwrap_or(&worktree_path);
    let parent_created = if !worktrees_parent.exists() {
        std::fs::create_dir_all(worktrees_parent).map_err(|e| {
            TaskMgrError::io_error(
                worktrees_parent.display().to_string(),
                "creating worktrees directory",
                e,
            )
        })?;
        true
    } else {
        false
    };

    // git worktree commands take the path as a positional argument; non-UTF-8
    // paths cannot round-trip through Command's &str API. Reject up front
    // with a clear error rather than silently passing the empty string and
    // letting git emit a confusing message.
    let worktree_path_str = worktree_path
        .to_str()
        .ok_or_else(|| TaskMgrError::InvalidState {
            resource_type: "worktree path".to_string(),
            id: worktree_path.display().to_string(),
            expected: "UTF-8 path".to_string(),
            actual: "non-UTF-8 bytes".to_string(),
        })?;

    // Check if branch exists
    let branch_exists = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", branch_name),
        ])
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "checking if branch exists",
                e,
            )
        })?
        .success();

    // Create worktree
    let create_result = if branch_exists {
        // Branch exists, create worktree for existing branch
        Command::new("git")
            .args(["worktree", "add", worktree_path_str, branch_name])
            .current_dir(project_root)
            .output()
            .map_err(|e| {
                TaskMgrError::io_error(
                    project_root.display().to_string(),
                    "running git worktree add",
                    e,
                )
            })?
    } else {
        // Branch doesn't exist, create new branch in worktree.
        // The `--` separator before start_point prevents flag injection from
        // malicious ref values (e.g. "--exec=...").
        let mut args = vec!["worktree", "add", "-b", branch_name, worktree_path_str];
        if let Some(sp) = start_point {
            args.push("--");
            args.push(sp);
        }
        Command::new("git")
            .args(&args)
            .current_dir(project_root)
            .output()
            .map_err(|e| {
                TaskMgrError::io_error(
                    project_root.display().to_string(),
                    "running git worktree add -b",
                    e,
                )
            })?
    };

    if !create_result.status.success() {
        let stderr = String::from_utf8_lossy(&create_result.stderr);

        // Clean up empty parent dir if we just created it (avoids orphan dirs).
        if parent_created {
            cleanup_empty_dir(worktrees_parent);
        }

        // Prune any stale worktree entries git may have recorded before failing.
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(project_root)
            .output();

        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: branch_name.to_string(),
            expected: "successful worktree creation".to_string(),
            actual: format!("git error: {}", stderr.trim()),
        });
    }

    eprintln!("Created worktree at {}", worktree_path.display());
    Ok(worktree_path)
}

/// Remove a git worktree.
///
/// Returns `Ok(true)` if the worktree was removed, `Ok(false)` if skipped due to
/// uncommitted changes, and `Err` if the path does not exist or git commands fail.
///
/// After removal, if the parent directory is empty (no other worktrees remain),
/// it is also removed.
pub fn remove_worktree(project_root: &Path, worktree_path: &Path) -> TaskMgrResult<bool> {
    if !worktree_path.exists() {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: worktree_path.display().to_string(),
            expected: "worktree path to exist".to_string(),
            actual: "path does not exist".to_string(),
        });
    }

    let path_str = worktree_path.to_string_lossy();
    let output = Command::new("git")
        .args(["worktree", "remove", path_str.as_ref()])
        .current_dir(project_root)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "running git worktree remove",
                e,
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // git exits non-zero with this message when the worktree has dirty changes
        if stderr.contains("contains modified or untracked files") {
            eprintln!(
                "warning: skipping removal of dirty worktree at {} (uncommitted changes)",
                worktree_path.display()
            );
            return Ok(false);
        }
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: worktree_path.display().to_string(),
            expected: "successful worktree removal".to_string(),
            actual: format!("git error: {}", stderr.trim()),
        });
    }

    // Prune stale worktree metadata from git's internal tracking
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(project_root)
        .output();

    // Remove empty parent dir (the {repo}-worktrees/ container)
    if let Some(parent) = worktree_path.parent() {
        cleanup_empty_dir(parent);
    }

    Ok(true)
}

// === Per-slot worktree management for parallel execution ===
//
// Git worktrees cannot share a branch — two worktrees pointing at the same
// branch is forbidden by git. For parallel execution, slot 0 uses the branch
// directly (the existing worktree created by `ensure_worktree`); slots 1+
// use ephemeral branches named `{branch}-slot-{N}` that are merged back into
// the main branch after each wave completes.

/// Marker block delimiting task-mgr's managed entries in `.git/info/attributes`.
/// The block is rewritten in place when its contents would change; any
/// user-authored lines outside the markers are preserved untouched.
const ATTR_MARKER_BEGIN: &str = "# task-mgr begin: progress union merge";
const ATTR_MARKER_END: &str = "# task-mgr end: progress union merge";

/// Body inserted between the markers. Uses git's built-in `union` merge driver
/// so concurrent appends to per-PRD progress files produce a concatenation
/// instead of a conflict. Patterns are anchored (they contain `/`) so they
/// only match progress files in the conventional locations.
const ATTR_BODY: &str =
    "tasks/progress*.txt merge=union\n.task-mgr/tasks/progress*.txt merge=union\n";

/// Resolve git's common directory (where `info/`, `config`, etc. live) for
/// `repo_path`. For linked worktrees this is the main repo's `.git`, so a
/// single write to `info/attributes` is visible to every slot worktree.
fn git_common_dir(repo_path: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("git rev-parse --git-common-dir spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse --git-common-dir failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return Err("git rev-parse --git-common-dir returned empty output".to_string());
    }
    let candidate = PathBuf::from(&raw);
    Ok(if candidate.is_absolute() {
        candidate
    } else {
        repo_path.join(candidate)
    })
}

/// Compute the new contents of `info/attributes` after ensuring the task-mgr
/// managed block is present and has the expected body. Returns `None` when
/// no rewrite is needed.
fn merged_attributes_contents(existing: &str) -> Option<String> {
    let desired_block = format!("{}\n{}{}\n", ATTR_MARKER_BEGIN, ATTR_BODY, ATTR_MARKER_END);

    if let (Some(begin), Some(end)) = (
        existing.find(ATTR_MARKER_BEGIN),
        existing.find(ATTR_MARKER_END),
    ) && begin < end
    {
        // End-of-line for the END marker — include the trailing newline if
        // present so we don't accumulate blank lines on repeated rewrites.
        let after_end = end + ATTR_MARKER_END.len();
        let after_end = if existing[after_end..].starts_with('\n') {
            after_end + 1
        } else {
            after_end
        };
        let current_block = &existing[begin..after_end];
        if current_block == desired_block {
            return None;
        }
        let mut rewritten = String::with_capacity(existing.len());
        rewritten.push_str(&existing[..begin]);
        rewritten.push_str(&desired_block);
        rewritten.push_str(&existing[after_end..]);
        return Some(rewritten);
    }

    // No marker block yet — append, ensuring exactly one blank line of
    // separation from any pre-existing content.
    let mut rewritten = String::with_capacity(existing.len() + desired_block.len() + 2);
    rewritten.push_str(existing);
    if !existing.is_empty() && !existing.ends_with('\n') {
        rewritten.push('\n');
    }
    if !existing.is_empty() {
        rewritten.push('\n');
    }
    rewritten.push_str(&desired_block);
    Some(rewritten)
}

/// Ensure `.git/info/attributes` declares `merge=union` for per-PRD progress
/// files so parallel slots can concurrently append without producing a merge
/// conflict on merge-back.
///
/// Per-clone (lives in `info/`, not committed). Idempotent — only rewrites
/// when the managed block is missing or its body has drifted. Failures are
/// non-fatal and surface to the caller as `Err` so the caller can decide
/// whether to log; merge-back will still attempt and conflicts (if any) will
/// surface there as before.
pub(crate) fn ensure_progress_union_merge(project_root: &Path) -> Result<(), String> {
    let common_dir = git_common_dir(project_root)?;
    let info_dir = common_dir.join("info");
    if !info_dir.exists() {
        std::fs::create_dir_all(&info_dir)
            .map_err(|e| format!("create {}: {}", info_dir.display(), e))?;
    }
    let attrs_path = info_dir.join("attributes");
    let existing = match std::fs::read_to_string(&attrs_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {}", attrs_path.display(), e)),
    };
    if let Some(updated) = merged_attributes_contents(&existing) {
        std::fs::write(&attrs_path, updated)
            .map_err(|e| format!("write {}: {}", attrs_path.display(), e))?;
    }
    Ok(())
}

/// Name of the ephemeral branch used by slot `N` (N > 0) for a loop running
/// on `branch_name`. Slot 0 uses the loop's own branch directly.
///
/// Single source of truth for ephemeral slot branch names — never construct
/// `{branch}-slot-{N}` inline (learning [1870]).
pub(crate) fn ephemeral_slot_branch(branch_name: &str, slot: usize) -> String {
    format!("{}-slot-{}", branch_name, slot)
}

/// Return the worktree path for the given slot.
///
/// Slot 0 returns the standard worktree path (same as `compute_worktree_path`);
/// slots 1+ receive a distinct suffixed directory (`{branch}-slot-{N}` after
/// branch-name sanitization) so git's one-branch-per-worktree rule is
/// satisfied.
pub(crate) fn compute_slot_worktree_path(
    project_root: &Path,
    branch_name: &str,
    slot: usize,
) -> PathBuf {
    if slot == 0 {
        compute_worktree_path(project_root, branch_name)
    } else {
        compute_worktree_path(project_root, &ephemeral_slot_branch(branch_name, slot))
    }
}

/// Create per-slot worktrees for parallel execution.
///
/// Slot 0 reuses (or creates) the branch's own worktree. Slots `1..num_slots`
/// create worktrees on ephemeral branches `{branch}-slot-{N}` forked from the
/// loop's main branch head (`branch_name`).
///
/// `project_root` must be the main repository path (not a worktree path) —
/// `ensure_worktree` rejects being invoked from a worktree whose branch does
/// not match the target.
///
/// Returns one PathBuf per slot, in slot-index order. Returns an empty vec
/// when `num_slots == 0`.
pub(crate) fn ensure_slot_worktrees(
    project_root: &Path,
    branch_name: &str,
    num_slots: usize,
) -> TaskMgrResult<Vec<PathBuf>> {
    if num_slots == 0 {
        return Ok(vec![]);
    }

    // Configure git's union merge for per-PRD progress files before any slot
    // commits exist. Concurrent slots both append to the same progress file;
    // without this the merge-back step conflicts on otherwise-compatible
    // line additions. Best-effort — a failure here is logged but does not
    // block worktree creation, since the rest of the merge can still succeed
    // when no slot touches a progress file.
    if num_slots > 1
        && let Err(e) = ensure_progress_union_merge(project_root)
    {
        eprintln!("Warning: failed to configure progress union merge: {}", e);
    }

    let mut paths = Vec::with_capacity(num_slots);

    // Slot 0: the loop's main branch worktree. `ensure_worktree` returns the
    // existing path when called from inside that worktree or when git already
    // has it registered.
    paths.push(ensure_worktree(project_root, branch_name, true, None)?);

    // Slots 1..N: ephemeral branches forked from the loop branch's head.
    // Forking from `branch_name` (not literal "main") ensures slots start
    // from the same base the loop is operating on.
    for slot in 1..num_slots {
        let ephemeral = ephemeral_slot_branch(branch_name, slot);
        let path = ensure_worktree(project_root, &ephemeral, true, Some(branch_name))?;
        paths.push(path);
    }

    Ok(paths)
}

/// Classification for one stale ephemeral slot branch found at startup.
///
/// `OrphanBranch` — branch ref exists, worktree directory is gone (e.g. user
/// removed it manually). Safe to delete the branch.
///
/// `CleanMerged` — clean worktree whose tip is an ancestor of the base branch.
/// All work has already merged forward; both branch and worktree can be
/// removed.
///
/// `CleanUnmerged` — clean worktree with commits NOT in the base branch.
/// Operator must decide; we never auto-delete.
///
/// `Dirty` — uncommitted changes or untracked files in the worktree.
/// Always blocks startup; never auto-cleaned.
#[derive(Debug)]
enum EphemeralCase {
    OrphanBranch,
    CleanMerged { worktree_path: PathBuf },
    CleanUnmerged { commits: Vec<String> },
    Dirty { worktree_path: PathBuf },
}

/// Reconcile stale `{branch_name}-slot-N` ephemeral branches/worktrees left
/// over from a prior loop crash.
///
/// Called from engine.rs **before** `ensure_slot_worktrees`. If a previous
/// run died after `ensure_slot_worktrees` but before `cleanup_slot_worktrees`,
/// stale ephemeral state can collide with the next run or silently keep
/// un-merged work invisible. This function classifies each stale branch into
/// one of four cases and either auto-cleans (cases 1, 2), warns (case 3), or
/// aborts startup (case 4 always; case 3 when `halt_threshold > 0`).
///
/// `halt_threshold` mirrors `ProjectConfig::merge_fail_halt_threshold`: a
/// non-zero value means "strict mode — block on any anomaly". Zero preserves
/// the legacy permissive posture for case 3 (warn but proceed); case 4
/// (dirty) always aborts because silently dropping uncommitted work is never
/// safe.
///
/// Idempotent: running twice in succession produces identical state on the
/// second pass — auto-deletions remove the only branches the second pass
/// would have considered, and case 3 / case 4 outcomes are pure observation.
///
/// Errors propagate only for the abort cases; per-branch classification or
/// deletion failures are logged via `eprintln!` and the pass continues with
/// the remaining branches so a single corrupt branch can never block all
/// hygiene.
pub(crate) fn reconcile_stale_ephemeral_slots(
    project_root: &Path,
    branch_name: &str,
    halt_threshold: u32,
) -> TaskMgrResult<()> {
    // Cheap up-front prune: if a worktree directory was deleted out-of-band,
    // git's internal registry may still hold a stale entry that prevents
    // `git branch -D` from succeeding. Pruning is best-effort.
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(project_root)
        .output();

    let pattern = format!("{}-slot-*", branch_name);
    // `--format='%(refname:short)'` returns bare branch names. Without it,
    // `git branch --list` prefixes lines with `* ` (current branch) or
    // `+ ` (branch checked out in another worktree) — those markers would
    // corrupt downstream classification.
    let output = Command::new("git")
        .args(["branch", "--list", "--format=%(refname:short)", &pattern])
        .current_dir(project_root)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "running git branch --list",
                e,
            )
        })?;

    if !output.status.success() {
        eprintln!(
            "warning: git branch --list failed during stale-ephemeral reconcile (continuing): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let branches: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if branches.is_empty() {
        return Ok(());
    }

    let mut unmerged: Vec<(String, Vec<String>)> = Vec::new();
    let mut dirty_paths: Vec<PathBuf> = Vec::new();

    for ephemeral in &branches {
        match classify_ephemeral_branch(project_root, branch_name, ephemeral) {
            Ok(EphemeralCase::OrphanBranch) => {
                if let Err(e) = delete_branch_force(project_root, ephemeral) {
                    eprintln!(
                        "warning: failed to delete orphan ephemeral branch {} (continuing): {}",
                        ephemeral, e
                    );
                } else {
                    eprintln!("Deleted orphan ephemeral branch {}", ephemeral);
                }
            }
            Ok(EphemeralCase::CleanMerged { worktree_path }) => {
                if let Err(e) = delete_merged_ephemeral(project_root, &worktree_path, ephemeral) {
                    eprintln!(
                        "warning: failed to clean already-merged ephemeral {} (continuing): {}",
                        ephemeral, e
                    );
                } else {
                    eprintln!(
                        "Cleaned already-merged ephemeral branch {} and its worktree",
                        ephemeral
                    );
                }
            }
            Ok(EphemeralCase::CleanUnmerged { commits }) => {
                unmerged.push((ephemeral.clone(), commits));
            }
            Ok(EphemeralCase::Dirty { worktree_path }) => {
                dirty_paths.push(worktree_path);
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to classify ephemeral branch {} during reconcile (continuing): {}",
                    ephemeral, e
                );
            }
        }
    }

    // Emit warnings for un-merged branches whether or not we're going to
    // abort — the operator needs to see the commit subjects either way.
    for (ephemeral, commits) in &unmerged {
        eprintln!(
            "warning: stale ephemeral branch {} has {} un-merged commit(s):",
            ephemeral,
            commits.len()
        );
        for subject in commits {
            eprintln!("    {}", subject);
        }
    }

    // Case 4: dirty worktrees always abort. They take precedence over
    // case 3 because the operator needs to inspect uncommitted work first.
    if !dirty_paths.is_empty() {
        let paths_str: Vec<String> = dirty_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        return Err(TaskMgrError::InvalidState {
            resource_type: "Stale ephemeral slot worktree".to_string(),
            id: paths_str.join(", "),
            expected: "clean state at startup".to_string(),
            actual: format!(
                "dirty worktree(s) detected: {}. \
                 Inspect manually, commit or discard changes, then `git worktree remove <path>` \
                 and `git branch -D <branch>` before re-running the loop.",
                paths_str.join(", ")
            ),
        });
    }

    // Case 3: un-merged ephemeral aborts only when halt_threshold > 0.
    // halt_threshold == 0 preserves the legacy permissive posture (warn-only).
    if halt_threshold > 0 && !unmerged.is_empty() {
        let names: Vec<String> = unmerged.iter().map(|(n, _)| n.clone()).collect();
        return Err(TaskMgrError::InvalidState {
            resource_type: "Stale ephemeral slot branch".to_string(),
            id: names.join(", "),
            expected: "no un-merged stale ephemeral branches at startup".to_string(),
            actual: format!(
                "un-merged ephemeral branch(es): {}. \
                 Inspect with `git log {base}..<branch>`, then either merge into {base} \
                 or `git branch -D <branch>` if the work should be discarded.",
                names.join(", "),
                base = branch_name
            ),
        });
    }

    Ok(())
}

/// Classify a single ephemeral slot branch into one of the four cases.
///
/// Returns `Err` when classification itself failed (git spawn / non-zero
/// exit). The caller logs and skips so a single sick branch cannot block
/// hygiene for the rest.
///
/// **Ancestor primitive**: uses `git merge-base --is-ancestor`, NOT
/// `git diff` — the latter compares trees and produces a false "un-merged"
/// classification once the base branch advances past a previously-merged
/// ephemeral.
fn classify_ephemeral_branch(
    project_root: &Path,
    base_branch: &str,
    ephemeral: &str,
) -> Result<EphemeralCase, String> {
    // Recover the slot index from the branch name to find its expected
    // worktree path. This is contract-bound to `ephemeral_slot_branch`'s
    // shape (worktree.rs:540) — anything that doesn't parse is treated as
    // a classification error so the caller skips it.
    let suffix = ephemeral
        .strip_prefix(&format!("{}-slot-", base_branch))
        .ok_or_else(|| "branch name does not match {base}-slot-N shape".to_string())?;
    let slot: usize = suffix
        .parse()
        .map_err(|e| format!("non-numeric slot suffix '{}': {}", suffix, e))?;

    let worktree_path = compute_slot_worktree_path(project_root, base_branch, slot);

    if !worktree_path.exists() {
        return Ok(EphemeralCase::OrphanBranch);
    }

    if is_worktree_dirty(&worktree_path)? {
        return Ok(EphemeralCase::Dirty { worktree_path });
    }

    if is_ancestor_of(project_root, ephemeral, base_branch)? {
        return Ok(EphemeralCase::CleanMerged { worktree_path });
    }

    let commits = list_commit_subjects_not_in_base(project_root, base_branch, ephemeral)?;
    Ok(EphemeralCase::CleanUnmerged { commits })
}

/// True iff `git status --porcelain` reports any modified, staged, or
/// untracked entries in `worktree_path`.
fn is_worktree_dirty(worktree_path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git status spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(!output.stdout.is_empty())
}

/// True iff `ancestor` is an ancestor of `descendant` per
/// `git merge-base --is-ancestor`.
///
/// Exit code 0 → ancestor; 1 → not ancestor; anything else → error. We
/// MUST NOT substitute `git diff` — it reports tree differences and would
/// false-positive on un-merged after the base branch advances past a
/// previously-merged ephemeral.
fn is_ancestor_of(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let status = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("git merge-base spawn: {}", e))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(c) => Err(format!("git merge-base --is-ancestor exit {}", c)),
        None => Err("git merge-base --is-ancestor terminated by signal".to_string()),
    }
}

/// Subjects of commits reachable from `ephemeral` but not from `base`.
fn list_commit_subjects_not_in_base(
    repo: &Path,
    base: &str,
    ephemeral: &str,
) -> Result<Vec<String>, String> {
    let range = format!("{}..{}", base, ephemeral);
    let output = Command::new("git")
        .args(["log", "--pretty=format:%s", &range])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git log spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Force-delete a branch from `project_root`. Tolerates "not found" so a
/// concurrent deletion (e.g. another reconcile pass mid-flight) does not
/// abort the outer loop.
fn delete_branch_force(project_root: &Path, branch: &str) -> Result<(), String> {
    let output = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git branch -D spawn: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") {
            return Ok(());
        }
        return Err(format!("git branch -D failed: {}", stderr.trim()));
    }
    Ok(())
}

/// Remove the worktree at `worktree_path` and then delete `ephemeral`.
///
/// Caller has already verified the worktree is clean; treat a dirty result
/// from `remove_worktree` as a defensive error rather than silently
/// swallowing because it would mean the dirty check raced.
fn delete_merged_ephemeral(
    project_root: &Path,
    worktree_path: &Path,
    ephemeral: &str,
) -> Result<(), String> {
    match remove_worktree(project_root, worktree_path) {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!(
                "worktree {} unexpectedly dirty during merged-cleanup",
                worktree_path.display()
            ));
        }
        Err(e) => return Err(format!("remove_worktree: {}", e)),
    }
    delete_branch_force(project_root, ephemeral)
}

/// Classifies why a slot's merge-back failed.
///
/// Carried on each `failed_slots` entry so engine.rs can choose appropriate
/// user-facing diagnostic text without string-sniffing the detail message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum SlotFailureKind {
    /// Failed before the resolver was invoked: rev-parse error on slot 0,
    /// git-merge spawn error, or fast-forward failure after the merge loop.
    #[default]
    PreResolver,
    /// The conflict resolver was invoked. The resolver may have failed,
    /// aborted, or claimed `Resolved` but failed post-resolution verification.
    ResolverAttempted,
}

/// Per-slot outcome returned by `merge_slot_branches`.
///
/// Slots that merged cleanly into the main branch land in `merged_slots`
/// (and get fast-forwarded). Slots whose merge or fast-forward failed land
/// in `failed_slots` along with diagnostic text and a [`SlotFailureKind`]
/// tag. On a merge conflict the function resets slot 0 to its pre-merge HEAD
/// (captured before each attempt) so subsequent slots and the next wave start
/// from a clean state.
#[derive(Debug, Default)]
pub(crate) struct MergeOutcomes {
    pub merged_slots: Vec<usize>,
    pub failed_slots: Vec<(usize, String, SlotFailureKind)>,
}

/// Capture the current HEAD commit-ish in `repo_path` for later restoration
/// or comparison.
///
/// Spawn or non-zero exit failures are returned as `Err(String)` so callers
/// can fold the diagnostic into their per-slot failure entries.
pub(crate) fn rev_parse_head(repo_path: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("git rev-parse spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Conflicted files in `slot0_path` after a non-zero `git merge` exit.
///
/// Uses `git diff --name-only --diff-filter=U` (unmerged paths). May return
/// `Ok(vec![])` if the merge failed before producing conflict markers (e.g.
/// pre-commit hook reject); callers must still treat that as "merge failed"
/// and invoke the resolver — the resolver can choose to abort.
pub(crate) fn list_conflicted_files(slot0_path: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(slot0_path)
        .output()
        .map_err(|e| format!("git diff spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Whether `slot0_path` is currently in the middle of a merge (MERGE_HEAD set).
///
/// Returns `Ok(true)` while a merge is in progress, `Ok(false)` once it's
/// resolved or aborted. Spawn failures (e.g. missing `git` binary) bubble up
/// as `Err(String)` so the caller can treat the merge state as unknown and
/// force a reset rather than silently accept a Resolved outcome.
pub(crate) fn has_unresolved_merge(slot0_path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "MERGE_HEAD"])
        .current_dir(slot0_path)
        .output()
        .map_err(|e| format!("git rev-parse spawn: {}", e))?;
    Ok(output.status.success())
}

/// Terminal outcome the merge-conflict resolver returns for a slot.
///
/// `Resolved` claims the resolver finished and committed; the merge function
/// re-inspects MERGE_HEAD and HEAD before trusting the claim. `Aborted`
/// means the resolver intentionally backed out (e.g. the conflict was too
/// large) and is expected to have already run `git merge --abort`. `Failed`
/// is a hard error (timeout, spawn failure, internal exception) and triggers
/// an unconditional reset to the pre-merge HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MergeResolverOutcome {
    Resolved,
    Aborted,
    Failed(String),
}

/// Inputs handed to a `MergeResolver` when slot 0's `git merge` exits
/// non-zero. Borrowed for the duration of a single resolver invocation; no
/// field outlives the merge loop iteration.
pub(crate) struct ResolverContext<'a> {
    pub slot: usize,
    pub slot0_path: &'a Path,
    pub ephemeral_branch: &'a str,
    pub conflicted_files: &'a [String],
    pub pre_merge_head: &'a str,
}

/// Strategy for resolving an in-progress merge conflict in slot 0.
///
/// Implementations are responsible for either driving the conflict to a
/// committed state (return `Resolved`), aborting the merge themselves
/// (return `Aborted`), or surfacing a hard error (return `Failed`). The
/// caller re-inspects MERGE_HEAD and HEAD afterwards, so a dishonest
/// `Resolved` is detected and downgraded to a failure.
pub(crate) trait MergeResolver {
    fn resolve(&self, ctx: ResolverContext<'_>) -> MergeResolverOutcome;
}

/// Test-only resolver that always reports failure. Used by unit tests that
/// want to exercise the conflict-handling pipeline without spawning Claude;
/// production callers always wire `ClaudeMergeResolver`.
#[cfg(test)]
pub(crate) struct NoOpResolver;

#[cfg(test)]
impl MergeResolver for NoOpResolver {
    fn resolve(&self, ctx: ResolverContext<'_>) -> MergeResolverOutcome {
        MergeResolverOutcome::Failed(format!("no resolver wired (slot {})", ctx.slot))
    }
}

/// Merge ephemeral slot branches back into the loop's main branch, invoking
/// `resolver` whenever `git merge --no-edit` exits non-zero.
///
/// Runs `git merge --no-edit {branch}-slot-{N}` from slot 0 for each slot in
/// `1..num_slots`. On a clean merge the slot lands in `merged_slots`. On a
/// non-zero merge exit, the function lists the conflicted files, hands them
/// (plus the ephemeral branch name and pre-merge HEAD) to `resolver`, then
/// re-inspects MERGE_HEAD and HEAD to verify the claimed outcome:
///   - `Resolved` only counts as a merge once MERGE_HEAD is cleared AND HEAD
///     has advanced past `pre_merge_head`. Anything else is downgraded to
///     `failed_slots` and forced back to `pre_merge_head`.
///   - `Aborted` always lands in `failed_slots` with a 'declined' diagnostic;
///     a defensive `git reset --hard pre_merge_head` runs if MERGE_HEAD is
///     still set (resolver forgot to abort) or HEAD drifted.
///   - `Failed(msg)` always resets to `pre_merge_head` and folds `msg` into
///     the diagnostic.
///
/// Resets use `git reset --hard <pre-merge>` rather than `git merge --abort`
/// because reset is idempotent regardless of how far the merge or resolver
/// progressed; abort can fail silently against a half-staged tree.
///
/// One bad slot does not poison the others — failures land in `failed_slots`
/// and the loop continues. Spawn-level failures are also captured per-slot.
/// After the merge loop, successfully-merged slots are fast-forwarded; if
/// their fast-forward fails the slot is demoted from `merged_slots` to
/// `failed_slots`.
pub(crate) fn merge_slot_branches_with_resolver(
    _project_root: &Path,
    branch_name: &str,
    num_slots: usize,
    resolver: &dyn MergeResolver,
    slot_paths: &[PathBuf],
) -> MergeOutcomes {
    let mut outcomes = MergeOutcomes::default();
    if num_slots <= 1 {
        return outcomes;
    }
    if slot_paths.len() < num_slots {
        for slot in 1..num_slots {
            outcomes.failed_slots.push((
                slot,
                format!(
                    "slot_paths has {} entries but num_slots is {}; cannot merge slot {}",
                    slot_paths.len(),
                    num_slots,
                    slot
                ),
                SlotFailureKind::PreResolver,
            ));
        }
        return outcomes;
    }

    let slot0_path = &slot_paths[0];

    // Slot-0 poisoning marker. Set when a `hard_reset` cleanup fails — at
    // that point HEAD may have drifted from any sensible base, so subsequent
    // slot iterations cannot trust their own `rev_parse_head` capture as a
    // reset target. Once poisoned, every remaining slot short-circuits to a
    // PreResolver failure with a clear diagnostic so the operator can see
    // why slots after the failure point did nothing.
    let mut slot0_poisoned: Option<String> = None;

    for slot in 1..num_slots {
        if let Some(ref reason) = slot0_poisoned {
            outcomes.failed_slots.push((
                slot,
                format!(
                    "skipped: slot 0 poisoned by earlier cleanup failure: {}",
                    reason
                ),
                SlotFailureKind::PreResolver,
            ));
            continue;
        }
        let ephemeral = ephemeral_slot_branch(branch_name, slot);
        let pre_merge_head = match rev_parse_head(slot0_path) {
            Ok(h) => h,
            Err(e) => {
                outcomes.failed_slots.push((
                    slot,
                    format!("rev-parse: {}", e),
                    SlotFailureKind::PreResolver,
                ));
                continue;
            }
        };
        let merge_result = Command::new("git")
            .args(["merge", "--no-edit", &ephemeral])
            .current_dir(slot0_path)
            .output();
        match merge_result {
            Ok(output) if output.status.success() => outcomes.merged_slots.push(slot),
            Ok(output) => {
                let detail = format_git_failure(&output.stdout, &output.stderr);
                match handle_conflict_for_slot(
                    slot,
                    slot0_path,
                    &ephemeral,
                    &pre_merge_head,
                    &detail,
                    resolver,
                ) {
                    Ok(()) => outcomes.merged_slots.push(slot),
                    Err((msg, kind)) => {
                        // PreResolver kind from handle_conflict_for_slot is the
                        // signal that a cleanup reset failed — poison slot 0.
                        if kind == SlotFailureKind::PreResolver
                            && msg.contains("reset cleanup failed")
                        {
                            slot0_poisoned = Some(msg.clone());
                        }
                        outcomes.failed_slots.push((slot, msg, kind));
                    }
                }
            }
            Err(e) => outcomes.failed_slots.push((
                slot,
                format!("git merge spawn: {}", e),
                SlotFailureKind::PreResolver,
            )),
        }
    }

    fast_forward_merged_slots(&mut outcomes, slot_paths, branch_name);
    outcomes
}

/// `git reset --hard <commit>` in `repo_path`. Returns `Err` with formatted
/// stderr/stdout context on spawn failure or non-zero exit so the caller can
/// poison slot 0 — a partially-failed reset can leave HEAD pointing at the
/// post-failed-resolve state (drifted from pre_merge_head), which would taint
/// every subsequent slot iteration's pre_merge_head capture.
fn hard_reset(repo_path: &Path, commit: &str) -> Result<(), String> {
    let output = Command::new("git")
        .args(["reset", "--hard", commit])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("git reset --hard spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git reset --hard {} failed: {}",
            commit,
            format_git_failure(&output.stdout, &output.stderr)
        ));
    }
    Ok(())
}

/// Verify HEAD advanced past `pre_merge_head` after MERGE_HEAD was cleared.
/// Returns `Ok(())` if HEAD moved forward, `Err(diagnostic)` if HEAD stayed
/// in place (resolver silently aborted) or `rev-parse` failed.
fn verify_resolver_advance(
    slot0_path: &Path,
    pre_merge_head: &str,
    detail: &str,
) -> Result<(), String> {
    match rev_parse_head(slot0_path) {
        Ok(post) if post == pre_merge_head => {
            Err(format!("{} | resolution did not advance HEAD", detail))
        }
        Ok(_) => Ok(()),
        Err(e) => Err(format!(
            "{} | post-resolver rev-parse failed: {}",
            detail, e
        )),
    }
}

/// Validate a `Resolved` claim: MERGE_HEAD must be gone AND HEAD must have
/// advanced. Hard-resets to `pre_merge_head` on any failure before returning
/// `Err((diagnostic, kind))`. Returns `Ok(())` on success.
///
/// If the recovery `hard_reset` itself fails, the kind escalates to
/// `PreResolver` and the diagnostic is annotated with the reset failure —
/// the caller is expected to treat that as slot-0 poisoning.
fn handle_resolved(
    slot0_path: &Path,
    pre_merge_head: &str,
    detail: &str,
) -> Result<(), (String, SlotFailureKind)> {
    let check = match has_unresolved_merge(slot0_path) {
        Err(e) => Err(format!(
            "{} | merge state unknown after resolver: {}",
            detail, e
        )),
        Ok(true) => Err(format!(
            "{} | resolver claimed resolution but MERGE_HEAD remains",
            detail
        )),
        Ok(false) => verify_resolver_advance(slot0_path, pre_merge_head, detail),
    };
    check.map_err(|e| {
        reset_or_escalate(
            slot0_path,
            pre_merge_head,
            e,
            SlotFailureKind::ResolverAttempted,
        )
    })
}

/// Defensive reset for the `Aborted` outcome: resets only when MERGE_HEAD is
/// still set or HEAD drifted, guarding against a resolver that forgot to abort.
/// Returns `Ok(())` if no reset was needed or it succeeded; `Err(reset_failure)`
/// if a reset was attempted and failed (caller poisons slot 0).
fn conditional_reset(slot0_path: &Path, pre_merge_head: &str) -> Result<(), String> {
    let stale = has_unresolved_merge(slot0_path).unwrap_or(true);
    let drifted = rev_parse_head(slot0_path)
        .map(|h| h != pre_merge_head)
        .unwrap_or(true);
    if stale || drifted {
        return hard_reset(slot0_path, pre_merge_head);
    }
    Ok(())
}

/// Run a defensive `hard_reset` and fold the original diagnostic together.
/// On reset success, returns `(original_msg, kind)`. On reset failure,
/// escalates to `PreResolver` (slot-0 poisoning marker) and appends the
/// reset error so the operator sees both failure modes.
fn reset_or_escalate(
    slot0_path: &Path,
    pre_merge_head: &str,
    original_msg: String,
    kind: SlotFailureKind,
) -> (String, SlotFailureKind) {
    match hard_reset(slot0_path, pre_merge_head) {
        Ok(()) => (original_msg, kind),
        Err(reset_err) => {
            eprintln!(
                "warning: slot-0 cleanup reset failed; subsequent slots will short-circuit: {}",
                reset_err
            );
            (
                format!("{} | reset cleanup failed: {}", original_msg, reset_err),
                SlotFailureKind::PreResolver,
            )
        }
    }
}

/// Invoke `resolver` on an in-progress conflict and verify the outcome.
/// Returns `Ok(())` on verified resolution or `Err((diagnostic, kind))` for
/// every failure mode. Delegates reset logic to `handle_resolved` (Resolved)
/// and `conditional_reset` (Aborted); always hard-resets for Failed.
///
/// `list_conflicted_files` errors are surfaced as `PreResolver` failures (the
/// merge probe itself is broken — the resolver can't be expected to do useful
/// work without knowing what's conflicted).
fn handle_conflict_for_slot(
    slot: usize,
    slot0_path: &Path,
    ephemeral: &str,
    pre_merge_head: &str,
    detail: &str,
    resolver: &dyn MergeResolver,
) -> Result<(), (String, SlotFailureKind)> {
    let conflicted = match list_conflicted_files(slot0_path) {
        Ok(v) => v,
        Err(probe_err) => {
            // S2 fix: don't silently coerce a probe failure into ResolverAttempted.
            // Reset to pre_merge_head and surface as PreResolver so the operator
            // knows the conflict resolution path was never even reached.
            return Err(reset_or_escalate(
                slot0_path,
                pre_merge_head,
                format!("{} | conflicted-files probe failed: {}", detail, probe_err),
                SlotFailureKind::PreResolver,
            ));
        }
    };
    let outcome = resolver.resolve(ResolverContext {
        slot,
        slot0_path,
        ephemeral_branch: ephemeral,
        conflicted_files: &conflicted,
        pre_merge_head,
    });
    match outcome {
        MergeResolverOutcome::Resolved => handle_resolved(slot0_path, pre_merge_head, detail),
        MergeResolverOutcome::Aborted => {
            let original_msg = format!("{} | resolver declined", detail);
            match conditional_reset(slot0_path, pre_merge_head) {
                Ok(()) => Err((original_msg, SlotFailureKind::ResolverAttempted)),
                Err(reset_err) => {
                    eprintln!(
                        "warning: slot-0 cleanup reset failed; subsequent slots will short-circuit: {}",
                        reset_err
                    );
                    Err((
                        format!("{} | reset cleanup failed: {}", original_msg, reset_err),
                        SlotFailureKind::PreResolver,
                    ))
                }
            }
        }
        MergeResolverOutcome::Failed(msg) => Err(reset_or_escalate(
            slot0_path,
            pre_merge_head,
            format!("{} | resolver failed: {}", detail, msg),
            SlotFailureKind::ResolverAttempted,
        )),
    }
}

/// Fast-forward each successfully-merged slot worktree to the updated main
/// branch head. Slots whose `--ff-only` fails are demoted to `failed_slots`.
fn fast_forward_merged_slots(
    outcomes: &mut MergeOutcomes,
    slot_paths: &[PathBuf],
    branch_name: &str,
) {
    let merged: Vec<usize> = outcomes.merged_slots.clone();
    for slot in merged {
        let slot_path = &slot_paths[slot];
        let ff_result = Command::new("git")
            .args(["merge", "--ff-only", branch_name])
            .current_dir(slot_path)
            .output();
        match ff_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let detail = format_git_failure(&output.stdout, &output.stderr);
                outcomes.merged_slots.retain(|s| *s != slot);
                outcomes
                    .failed_slots
                    .push((slot, detail, SlotFailureKind::PreResolver));
            }
            Err(e) => {
                outcomes.merged_slots.retain(|s| *s != slot);
                outcomes.failed_slots.push((
                    slot,
                    format!("git ff spawn: {}", e),
                    SlotFailureKind::PreResolver,
                ));
            }
        }
    }
}

/// Pick the most informative non-empty git output for a failure summary.
/// `git merge` writes conflict details to stdout and short context to stderr;
/// either may be empty depending on the failure mode.
fn format_git_failure(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout_str = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr_str = String::from_utf8_lossy(stderr).trim().to_string();
    match (stdout_str.is_empty(), stderr_str.is_empty()) {
        (false, false) => format!("{} | {}", stderr_str, stdout_str),
        (false, true) => stdout_str,
        (true, false) => stderr_str,
        (true, true) => "git failed without output".to_string(),
    }
}

/// Remove slot worktrees (slots 1+) and delete their ephemeral branches.
///
/// Slot 0 is the loop's main branch worktree and is always preserved.
///
/// A dirty slot worktree (uncommitted changes) is left intact and a warning
/// is printed — its ephemeral branch is then also preserved, since
/// `git branch -D` refuses to delete a branch that is still checked out.
/// Missing ephemeral branches (e.g., partial setup) are tolerated.
pub(crate) fn cleanup_slot_worktrees(
    project_root: &Path,
    branch_name: &str,
    num_slots: usize,
) -> TaskMgrResult<()> {
    // Track which slots were successfully detached so we only attempt branch
    // deletion for those.
    let mut removed_slots = Vec::new();

    for slot in 1..num_slots {
        let slot_path = compute_slot_worktree_path(project_root, branch_name, slot);
        if !slot_path.exists() {
            // Worktree was never created or was already cleaned up — its
            // ephemeral branch (if any) is still eligible for deletion.
            removed_slots.push(slot);
            continue;
        }

        match remove_worktree(project_root, &slot_path)? {
            true => removed_slots.push(slot),
            false => {
                // `remove_worktree` already warned; skip branch deletion
                // because the worktree still has the branch checked out.
                eprintln!(
                    "warning: skipping ephemeral branch deletion for dirty slot {}",
                    slot
                );
            }
        }
    }

    for slot in removed_slots {
        let ephemeral = ephemeral_slot_branch(branch_name, slot);
        let output = Command::new("git")
            .args(["branch", "-D", &ephemeral])
            .current_dir(project_root)
            .output()
            .map_err(|e| {
                TaskMgrError::io_error(
                    project_root.display().to_string(),
                    "running git branch -D",
                    e,
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Tolerate branches that never existed (e.g., ensure_slot_worktrees
            // was not called for this slot). Any other failure is fatal.
            if !stderr.contains("not found") {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "Git branch".to_string(),
                    id: ephemeral.clone(),
                    expected: format!("deletion of ephemeral branch {}", ephemeral),
                    actual: format!("git error: {}", stderr.trim()),
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::test_utils::setup_git_repo_with_file;
    use std::fs;
    use std::process::Command;

    #[test]
    fn test_sanitize_branch_name_replaces_slashes() {
        assert_eq!(sanitize_branch_name("feature/auth"), "feature-auth");
        assert_eq!(
            sanitize_branch_name("feat/user/auth-v2"),
            "feat-user-auth-v2"
        );
    }

    #[test]
    fn test_sanitize_branch_name_replaces_spaces() {
        assert_eq!(sanitize_branch_name("my branch"), "my-branch");
        assert_eq!(sanitize_branch_name("my  branch"), "my--branch");
    }

    #[test]
    fn test_sanitize_branch_name_replaces_windows_forbidden_chars() {
        assert_eq!(sanitize_branch_name("a:b*c?d"), "a-b-c-d");
        assert_eq!(sanitize_branch_name("a<b>c|d"), "a-b-c-d");
        assert_eq!(sanitize_branch_name("a\"b\\c"), "a-b-c");
    }

    #[test]
    fn test_sanitize_branch_name_preserves_valid_chars() {
        assert_eq!(sanitize_branch_name("simple-branch"), "simple-branch");
        assert_eq!(sanitize_branch_name("branch_name"), "branch_name");
        assert_eq!(sanitize_branch_name("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn test_compute_worktree_path_basic() {
        let project_root = Path::new("/home/user/myproject");
        let path = compute_worktree_path(project_root, "feature/auth");

        assert_eq!(
            path,
            PathBuf::from("/home/user/myproject-worktrees/feature-auth")
        );
    }

    #[test]
    fn test_compute_worktree_path_simple_branch() {
        let project_root = Path::new("/home/user/myproject");
        let path = compute_worktree_path(project_root, "main");

        assert_eq!(path, PathBuf::from("/home/user/myproject-worktrees/main"));
    }

    #[test]
    fn test_parse_worktree_list_empty() {
        let output = "";
        let worktrees = parse_worktree_list(output);
        assert!(worktrees.is_empty());
    }

    #[test]
    fn test_parse_worktree_list_single_worktree() {
        let output = "worktree /home/user/project\nHEAD abc123\nbranch refs/heads/main\n";
        let worktrees = parse_worktree_list(output);

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].0, PathBuf::from("/home/user/project"));
        assert_eq!(worktrees[0].1, Some("main".to_string()));
    }

    #[test]
    fn test_parse_worktree_list_multiple_worktrees() {
        let output = "\
worktree /home/user/project
HEAD abc123
branch refs/heads/main

worktree /home/user/project-worktrees/feature-auth
HEAD def456
branch refs/heads/feature/auth

worktree /home/user/project-worktrees/detached
HEAD ghi789
detached
";
        let worktrees = parse_worktree_list(output);

        assert_eq!(worktrees.len(), 3);
        assert_eq!(worktrees[0].0, PathBuf::from("/home/user/project"));
        assert_eq!(worktrees[0].1, Some("main".to_string()));
        assert_eq!(
            worktrees[1].0,
            PathBuf::from("/home/user/project-worktrees/feature-auth")
        );
        assert_eq!(worktrees[1].1, Some("feature/auth".to_string()));
        assert_eq!(
            worktrees[2].0,
            PathBuf::from("/home/user/project-worktrees/detached")
        );
        assert_eq!(worktrees[2].1, None); // detached HEAD has no branch
    }

    #[test]
    fn test_ensure_worktree_creates_new_worktree() {
        let tmp = setup_git_repo_with_file();

        // Create a new worktree for a new branch
        let result = ensure_worktree(tmp.path(), "feature/test-wt", true, None);
        assert!(
            result.is_ok(),
            "Should create worktree for new branch: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        assert!(
            wt_path.exists(),
            "Worktree path should exist: {}",
            wt_path.display()
        );
        assert!(
            wt_path.join(".git").exists(),
            "Worktree should have .git file"
        );

        // Verify the worktree is on the correct branch
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(current, "feature/test-wt");
    }

    #[test]
    fn test_ensure_worktree_reuses_existing_worktree() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/reuse-test", true, None);
        assert!(result1.is_ok());
        let wt_path1 = result1.unwrap();

        // Call again - should reuse the same worktree
        let result2 = ensure_worktree(tmp.path(), "feature/reuse-test", true, None);
        assert!(result2.is_ok());
        let wt_path2 = result2.unwrap();

        assert_eq!(
            wt_path1, wt_path2,
            "Should return same path for existing worktree"
        );
    }

    #[test]
    fn test_ensure_worktree_for_existing_branch() {
        let tmp = setup_git_repo_with_file();

        // Create a branch without a worktree
        Command::new("git")
            .args(["branch", "existing-branch"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        // Create worktree for the existing branch
        let result = ensure_worktree(tmp.path(), "existing-branch", true, None);
        assert!(
            result.is_ok(),
            "Should create worktree for existing branch: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(current, "existing-branch");
    }

    #[test]
    fn test_ensure_worktree_path_contains_sanitized_branch_name() {
        let tmp = setup_git_repo_with_file();

        let result = ensure_worktree(tmp.path(), "feature/nested/branch", true, None);
        assert!(result.is_ok());

        let wt_path = result.unwrap();
        let path_str = wt_path.to_string_lossy();

        // Path should have sanitized branch name (slashes -> dashes)
        assert!(
            path_str.contains("feature-nested-branch"),
            "Worktree path should contain sanitized branch name, got: {}",
            path_str
        );
    }

    #[test]
    fn test_ensure_worktree_from_inside_correct_worktree_returns_same_path() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/inside-test", true, None);
        assert!(result1.is_ok());
        let wt_path = result1.unwrap();

        // Now call ensure_worktree from inside the worktree for the same branch
        let result2 = ensure_worktree(&wt_path, "feature/inside-test", true, None);
        assert!(
            result2.is_ok(),
            "Should succeed when called from inside correct worktree: {:?}",
            result2.err()
        );

        assert_eq!(
            result2.unwrap(),
            wt_path,
            "Should return the worktree path when called from inside it"
        );
    }

    #[test]
    fn test_ensure_worktree_from_inside_wrong_worktree_fails() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/wt-one", true, None);
        assert!(result1.is_ok());
        let wt_path = result1.unwrap();

        // Now call ensure_worktree from inside the worktree but for a different branch
        let result2 = ensure_worktree(&wt_path, "feature/wt-two", true, None);
        assert!(
            result2.is_err(),
            "Should fail when called from inside worktree for wrong branch"
        );

        let err = result2.unwrap_err().to_string();
        assert!(
            err.contains("already inside worktree"),
            "Error should mention being inside a worktree, got: {}",
            err
        );
    }

    #[test]
    fn test_is_inside_worktree_false_for_main_repo() {
        let tmp = setup_git_repo_with_file();

        let result = is_inside_worktree(tmp.path());
        assert!(result.is_ok());
        assert!(
            !result.unwrap(),
            "Main repo should not be detected as worktree"
        );
    }

    #[test]
    fn test_is_inside_worktree_true_for_actual_worktree() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/detect-test", true, None);
        assert!(result1.is_ok());
        let wt_path = result1.unwrap();

        let result = is_inside_worktree(&wt_path);
        assert!(result.is_ok());
        assert!(result.unwrap(), "Worktree should be detected as worktree");
    }

    // --- TEST-INIT-001: remove_worktree() and early exit cleanup ---

    #[test]
    fn test_remove_worktree_clean_returns_true_and_path_removed() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree to remove
        let wt_path =
            ensure_worktree(tmp.path(), "feature/cleanup-me", true, None).expect("create worktree");
        assert!(wt_path.exists(), "Worktree should exist before removal");

        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_ok(),
            "remove_worktree on clean worktree should return Ok: {:?}",
            result.err()
        );
        assert!(
            result.unwrap(),
            "remove_worktree on clean worktree should return Ok(true)"
        );
        assert!(
            !wt_path.exists(),
            "Worktree path should no longer exist after removal"
        );
    }

    #[test]
    fn test_remove_worktree_dirty_returns_false_and_path_preserved() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let wt_path =
            ensure_worktree(tmp.path(), "feature/dirty-wt", true, None).expect("create worktree");

        // Dirty the worktree
        fs::write(wt_path.join("dirty.txt"), "uncommitted content").expect("write dirty file");

        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_ok(),
            "remove_worktree on dirty worktree should return Ok (skip with warning): {:?}",
            result.err()
        );
        assert!(
            !result.unwrap(),
            "remove_worktree on dirty worktree should return Ok(false)"
        );
        assert!(
            wt_path.exists(),
            "Dirty worktree path should still exist (was skipped)"
        );
    }

    #[test]
    fn test_remove_worktree_removes_empty_parent_dir() {
        let tmp = setup_git_repo_with_file();

        // Create a single worktree (will be the only one in the parent dir)
        let wt_path =
            ensure_worktree(tmp.path(), "feature/last-wt", true, None).expect("create worktree");
        let parent = wt_path
            .parent()
            .expect("worktree has parent dir")
            .to_path_buf();
        assert!(parent.exists(), "Parent dir should exist");

        // Remove the only worktree — parent should be removed too (now empty)
        let result = remove_worktree(tmp.path(), &wt_path).expect("remove_worktree should succeed");
        assert!(result, "Should have removed the worktree");

        assert!(
            !parent.exists(),
            "Empty parent dir should be removed after last worktree is gone: {:?}",
            parent
        );
    }

    #[test]
    fn test_remove_worktree_non_empty_parent_dir_preserved() {
        let tmp = setup_git_repo_with_file();

        // Create two worktrees in the same parent dir
        let wt1 = ensure_worktree(tmp.path(), "feature/wt-alpha", true, None).expect("create wt1");
        let wt2 = ensure_worktree(tmp.path(), "feature/wt-beta", true, None).expect("create wt2");

        let parent = wt1.parent().expect("wt1 has parent").to_path_buf();
        assert_eq!(
            wt1.parent().unwrap(),
            wt2.parent().unwrap(),
            "Both worktrees should share a parent dir"
        );

        // Remove only one — parent should NOT be removed (wt2 still there)
        let result = remove_worktree(tmp.path(), &wt1).expect("remove wt1");
        assert!(result, "Should have removed wt1");

        assert!(
            parent.exists(),
            "Parent dir should NOT be removed when other worktrees remain"
        );
        assert!(wt2.exists(), "wt2 should still exist");
    }

    // Known-bad discriminator: non-existent path is an error, not Ok(true)
    #[test]
    fn test_remove_worktree_non_existent_path_returns_error() {
        let tmp = setup_git_repo_with_file();

        let nonexistent = tmp.path().join("does-not-exist");
        assert!(!nonexistent.exists(), "Path should not exist for this test");

        let result = remove_worktree(tmp.path(), &nonexistent);
        assert!(
            result.is_err(),
            "remove_worktree with non-existent path should return Err, not Ok(true): {:?}",
            result.ok()
        );
    }

    #[test]
    fn test_ensure_worktree_cleans_up_empty_parent_on_git_add_failure() {
        // When git worktree add fails after creating the parent dir,
        // ensure_worktree should remove the empty parent dir (not leave orphan dirs).
        //
        // To trigger git worktree add failure: use a branch name containing ".."
        // which git forbids in ref names (ref name rules: no consecutive dots).
        let tmp = setup_git_repo_with_file();

        // "feature/bad..ref" contains ".." which git rejects as an invalid ref name.
        // The parent dir {repo}-worktrees/ will be created by ensure_worktree
        // (it won't pre-exist) and must be removed after the git failure.
        let branch = "feature/bad..ref";
        let wt_path = compute_worktree_path(tmp.path(), branch);
        let parent = wt_path.parent().expect("has parent").to_path_buf();

        // Parent must not exist before the call (so ensure_worktree creates it).
        assert!(
            !parent.exists(),
            "parent dir should not pre-exist before the test"
        );

        let result = ensure_worktree(tmp.path(), branch, true, None);
        assert!(
            result.is_err(),
            "ensure_worktree with invalid git ref name should fail"
        );

        // Parent dir must not be left as an orphan
        assert!(
            !parent.exists(),
            "Empty parent dir should be cleaned up after git worktree add failure: {:?}",
            parent
        );
    }

    // --- TEST-001: Comprehensive tests for remove_worktree() and early exit cleanup ---

    /// Test: remove_worktree() on worktree with staged but uncommitted changes.
    /// Staged changes are "modified" from git's perspective, so `git worktree remove`
    /// should refuse and return Ok(false) (skip with warning).
    #[test]
    fn test_remove_worktree_staged_changes_returns_false() {
        let tmp = setup_git_repo_with_file();

        let wt_path = ensure_worktree(tmp.path(), "feature/staged-changes", true, None)
            .expect("create worktree");

        // Stage a new file in the worktree without committing
        let new_file = wt_path.join("staged.txt");
        fs::write(&new_file, "staged content").expect("write staged file");
        Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(&wt_path)
            .output()
            .expect("git add");

        // Verify the file is staged
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&wt_path)
            .output()
            .expect("git status");
        let status_str = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_str.contains("staged.txt"),
            "File should be staged: {}",
            status_str
        );

        // remove_worktree should skip dirty (staged) worktree
        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_ok(),
            "remove_worktree with staged changes should return Ok (skip): {:?}",
            result.err()
        );
        assert!(
            !result.unwrap(),
            "remove_worktree with staged changes should return Ok(false)"
        );
        assert!(
            wt_path.exists(),
            "Worktree with staged changes should be preserved"
        );
    }

    /// Test: remove_worktree() when the worktree path was deleted out-of-band (directory is gone
    /// but git may still have it in its worktree list). The path no longer exists on disk.
    #[test]
    fn test_remove_worktree_out_of_band_delete_returns_error() {
        let tmp = setup_git_repo_with_file();

        // Create a real worktree first so git knows about it
        let wt_path = ensure_worktree(tmp.path(), "feature/out-of-band", true, None)
            .expect("create worktree");
        assert!(wt_path.exists(), "Worktree should exist initially");

        // Simulate out-of-band deletion: manually remove the directory without going through git
        fs::remove_dir_all(&wt_path).expect("manual rm -rf of worktree dir");
        assert!(
            !wt_path.exists(),
            "Worktree directory should be gone after manual delete"
        );

        // remove_worktree should return Err because the path is already gone
        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_err(),
            "remove_worktree on out-of-band-deleted path should return Err, got: {:?}",
            result.ok()
        );
    }

    /// Test: parent dir is preserved when it contains a regular file (non-empty for reasons
    /// other than worktrees). cleanup_empty_dir is best-effort and must not remove non-empty dirs.
    #[test]
    fn test_remove_worktree_parent_with_extra_file_not_removed() {
        let tmp = setup_git_repo_with_file();

        let wt_path = ensure_worktree(tmp.path(), "feature/wt-with-sibling", true, None)
            .expect("create worktree");
        let parent = wt_path.parent().expect("worktree has parent").to_path_buf();

        // Place a regular file in the parent dir (simulates user-created content)
        let extra_file = parent.join("README.txt");
        fs::write(&extra_file, "some user content").expect("write extra file");

        // Remove the worktree
        let result = remove_worktree(tmp.path(), &wt_path).expect("remove worktree");
        assert!(result, "Should have removed the worktree");

        // Parent dir must NOT be removed because it still has README.txt
        assert!(
            parent.exists(),
            "Parent dir should be preserved when it contains extra files"
        );
        assert!(
            extra_file.exists(),
            "Extra file in parent should be preserved"
        );
    }

    /// Parameterized-style test: remove_worktree() behavior for various git states.
    /// Tests clean worktree (Ok(true)), dirty with untracked (Ok(false)),
    /// and dirty with modified tracked file (Ok(false)).
    #[test]
    fn test_remove_worktree_git_state_table() {
        struct TestCase {
            name: &'static str,
            // mutate fn: receives the worktree path, sets up git state
            setup: fn(&std::path::Path),
            expected_ok: bool,
            expected_value: bool,
            path_removed: bool,
        }

        let cases: &[TestCase] = &[
            TestCase {
                name: "clean worktree",
                setup: |_| {},
                expected_ok: true,
                expected_value: true,
                path_removed: true,
            },
            TestCase {
                name: "dirty: untracked file",
                setup: |wt| {
                    fs::write(wt.join("new_untracked.txt"), "data").expect("write untracked");
                },
                expected_ok: true,
                expected_value: false,
                path_removed: false,
            },
            TestCase {
                name: "dirty: modified tracked file",
                setup: |wt| {
                    // file.txt was committed in setup_git_repo via the main repo,
                    // but the worktree has its own copy of the repo state.
                    // We need to create a file that was previously committed in this worktree.
                    fs::write(wt.join("new_tracked.txt"), "original").expect("write tracked");
                    Command::new("git")
                        .args(["add", "new_tracked.txt"])
                        .current_dir(wt)
                        .output()
                        .expect("git add");
                    Command::new("git")
                        .args(["commit", "-m", "add tracked"])
                        .current_dir(wt)
                        .output()
                        .expect("git commit");
                    // Now modify it without committing
                    fs::write(wt.join("new_tracked.txt"), "modified").expect("modify tracked");
                },
                expected_ok: true,
                expected_value: false,
                path_removed: false,
            },
        ];

        for case in cases {
            let tmp = setup_git_repo_with_file();
            let branch = format!("feature/state-test-{}", case.name.replace([':', ' '], "-"));
            let wt_path = ensure_worktree(tmp.path(), &branch, true, None)
                .unwrap_or_else(|e| panic!("[{}] create worktree: {:?}", case.name, e));

            (case.setup)(&wt_path);

            let result = remove_worktree(tmp.path(), &wt_path);
            assert_eq!(
                result.is_ok(),
                case.expected_ok,
                "[{}] expected is_ok()={}, got: {:?}",
                case.name,
                case.expected_ok,
                result
            );
            if case.expected_ok {
                assert_eq!(
                    result.unwrap(),
                    case.expected_value,
                    "[{}] expected Ok({})",
                    case.name,
                    case.expected_value
                );
            }
            assert_eq!(
                !wt_path.exists(),
                case.path_removed,
                "[{}] path_removed={} but path exists={}",
                case.name,
                case.path_removed,
                wt_path.exists()
            );
        }
    }

    #[test]
    fn test_ensure_worktree_runs_git_prune_on_partial_failure() {
        // When git worktree add creates a partial entry then fails, ensure_worktree
        // should call `git worktree prune` so stale entries don't accumulate.
        //
        // This is difficult to trigger deterministically (requires a mid-operation
        // failure). The test validates the behavior via state inspection: after a
        // forced failure, `git worktree list` should not contain stale entries.
        let tmp = setup_git_repo_with_file();

        // Simulate partial failure: create the worktree directory with content so
        // git worktree add refuses it (git rejects non-empty target directories).
        let wt_path = compute_worktree_path(tmp.path(), "feature/prune-test");
        let parent = wt_path.parent().expect("has parent");
        fs::create_dir_all(&wt_path).expect("create dir to cause conflict");
        // Put a file inside so git refuses to use this non-empty directory.
        fs::write(wt_path.join("dummy.txt"), "block").expect("write dummy file");

        let result = ensure_worktree(tmp.path(), "feature/prune-test", true, None);
        assert!(
            result.is_err(),
            "ensure_worktree should fail when directory already exists"
        );

        // After failure, run git worktree list and verify no stale entry for the path
        let list_output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .expect("git worktree list");
        let list_str = String::from_utf8_lossy(&list_output.stdout);

        // The failed worktree should not appear in the list (prune was called)
        let wt_str = wt_path.to_string_lossy();
        assert!(
            !list_str.contains(wt_str.as_ref()),
            "Stale worktree entry should be pruned after partial failure, got list: {}",
            list_str
        );

        // Clean up
        let _ = fs::remove_dir_all(parent);
    }

    // --- CHAIN-001: start_point parameter tests ---

    /// Regression test: start_point=None must produce identical behavior to before the
    /// parameter was added (new branch from HEAD).
    #[test]
    fn test_ensure_worktree_start_point_none_creates_from_head() {
        let tmp = setup_git_repo_with_file();

        let result = ensure_worktree(tmp.path(), "feat/from-head", true, None);
        assert!(
            result.is_ok(),
            "start_point=None should create worktree from HEAD: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        assert!(wt_path.exists(), "Worktree path should exist");

        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(current, "feat/from-head");
    }

    /// start_point=Some("branch-a") must create the new branch rooted at branch-a's commits.
    /// The `--` separator in git args is what enables this (prevents flag injection).
    #[test]
    fn test_ensure_worktree_start_point_some_creates_branch_from_ref() {
        let tmp = setup_git_repo_with_file();

        // Create branch-a and add a distinguishing commit on it.
        Command::new("git")
            .args(["checkout", "-b", "branch-a"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout -b branch-a");
        fs::write(tmp.path().join("branch-a-marker.txt"), "branch-a content")
            .expect("write marker");
        Command::new("git")
            .args(["add", "branch-a-marker.txt"])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "branch-a unique commit"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit on branch-a");

        // Return to main so we can create the new worktree from the main repo.
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout main");

        // Create branch-b from branch-a via ensure_worktree with start_point.
        let result = ensure_worktree(tmp.path(), "feat/branch-b", true, Some("branch-a"));
        assert!(
            result.is_ok(),
            "start_point=Some('branch-a') should succeed: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        // Verify branch-b worktree contains branch-a's distinguishing file.
        assert!(
            wt_path.join("branch-a-marker.txt").exists(),
            "branch-b should contain branch-a-marker.txt (start_point was branch-a)"
        );
        // Verify it is a new branch, not a checkout of branch-a.
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(
            current, "feat/branch-b",
            "Worktree should be on branch feat/branch-b, not branch-a"
        );
    }

    /// When the branch already exists, start_point must be silently ignored.
    /// The existing-branch git command path (`git worktree add <path> <branch>`) does
    /// not receive start_point — this test confirms no error is raised and the result
    /// is on the existing branch.
    #[test]
    fn test_ensure_worktree_start_point_ignored_for_existing_branch() {
        let tmp = setup_git_repo_with_file();

        // Create branch "preexisting" without a worktree.
        Command::new("git")
            .args(["branch", "preexisting"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        // Call with start_point=Some("nonexistent-ref") — if start_point were passed to
        // the existing-branch path it would cause a git error; succeeding proves it's ignored.
        let result = ensure_worktree(tmp.path(), "preexisting", true, Some("nonexistent-ref"));
        assert!(
            result.is_ok(),
            "start_point should be ignored when branch already exists: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(
            current, "preexisting",
            "Worktree should be on the existing branch"
        );
    }

    /// Integration test: creating a worktree with `start_point` from another branch
    /// must cause the new worktree to contain the source branch's commits.
    ///
    /// Known-bad discriminator: if `start_point` is silently ignored, `feat/phase-2`
    /// would branch from HEAD (main) and would NOT contain `phase1-marker.txt` —
    /// this test catches that failure.
    #[test]
    fn test_ensure_worktree_chain_preserves_commits() {
        let tmp = setup_git_repo_with_file();

        // Step 1: create feat/phase-1 worktree from HEAD (main)
        let phase1_path = ensure_worktree(tmp.path(), "feat/phase-1", true, None)
            .expect("create feat/phase-1 worktree");
        assert!(phase1_path.exists(), "phase-1 worktree must exist on disk");

        // Step 2: make a unique commit in the phase-1 worktree
        fs::write(phase1_path.join("phase1-marker.txt"), "phase1 content")
            .expect("write phase1-marker.txt");
        Command::new("git")
            .args(["add", "phase1-marker.txt"])
            .current_dir(&phase1_path)
            .output()
            .expect("git add phase1-marker.txt");
        Command::new("git")
            .args(["commit", "-m", "phase-1 unique commit"])
            .current_dir(&phase1_path)
            .output()
            .expect("git commit in phase-1");

        // Step 3: create feat/phase-2 branched from feat/phase-1 via start_point
        let phase2_path = ensure_worktree(tmp.path(), "feat/phase-2", true, Some("feat/phase-1"))
            .expect("create feat/phase-2 worktree from feat/phase-1");
        assert!(phase2_path.exists(), "phase-2 worktree must exist on disk");

        // Acceptance: phase-2 must contain phase-1's marker file
        assert!(
            phase2_path.join("phase1-marker.txt").exists(),
            "phase-2 must contain phase1-marker.txt (inherited via start_point=feat/phase-1)"
        );

        // Acceptance: phase-2's git log must include phase-1's commit message
        let log_output = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&phase2_path)
            .output()
            .expect("git log in phase-2");
        let log_str = String::from_utf8_lossy(&log_output.stdout);
        assert!(
            log_str.contains("phase-1 unique commit"),
            "phase-2 git log must contain phase-1's commit message, got: {}",
            log_str
        );

        // Negative: phase-2 must be a NEW branch, not a detached HEAD or checkout of phase-1
        let branch_output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&phase2_path)
            .output()
            .expect("git rev-parse HEAD in phase-2");
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();
        assert_eq!(
            branch, "feat/phase-2",
            "phase-2 worktree must be on branch 'feat/phase-2', not '{}'",
            branch
        );
    }

    // --- TEST-INIT-002: per-slot worktree management ---
    //
    // TDD tests defining the contract for ensure_slot_worktrees,
    // merge_slot_branches, and cleanup_slot_worktrees. Tests that invoke
    // the stubs are #[ignore]'d until FEAT-008 implements the bodies.
    // Tests that exercise git's own constraints run immediately.

    /// Helper: fetch the commit-ish for the given ref inside `repo_root`.
    fn rev_parse(repo_root: &std::path::Path, refname: &str) -> String {
        let out = Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(repo_root)
            .output()
            .expect("git rev-parse");
        assert!(
            out.status.success(),
            "git rev-parse {} failed: {}",
            refname,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// AC1: ensure_slot_worktrees creates N worktrees on ephemeral branches
    #[test]
    fn test_ensure_slot_worktrees_creates_n_worktrees() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/parallel";

        // Slot 0 pre-existing (the user's normal branch worktree)
        let _slot0 = ensure_worktree(tmp.path(), branch, true, None).expect("slot 0");

        let paths = ensure_slot_worktrees(tmp.path(), branch, 3).expect("ensure_slot_worktrees");
        assert_eq!(paths.len(), 3, "should return one path per slot");

        for (i, path) in paths.iter().enumerate() {
            assert!(
                path.exists(),
                "slot {} worktree path should exist on disk: {}",
                i,
                path.display()
            );
            assert!(
                path.join(".git").exists(),
                "slot {} path should have a .git entry",
                i
            );
        }
    }

    /// AC2: slot 0 reuses existing branch worktree
    #[test]
    fn test_ensure_slot_worktrees_slot_zero_reuses_existing_worktree() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/slot-zero";

        let existing =
            ensure_worktree(tmp.path(), branch, true, None).expect("pre-create slot 0 worktree");

        let paths = ensure_slot_worktrees(tmp.path(), branch, 2).expect("ensure_slot_worktrees");
        assert_eq!(
            paths[0].canonicalize().unwrap(),
            existing.canonicalize().unwrap(),
            "slot 0 must reuse the existing branch worktree, not create a new one"
        );

        let slot0_branch = get_current_branch(&paths[0]).expect("branch of slot 0");
        assert_eq!(slot0_branch, branch, "slot 0 must stay on the main branch");
    }

    /// AC3: slots 1+ paths sit at {repo}-worktrees/{branch}-slot-{N}/
    #[test]
    fn test_compute_slot_worktree_path_slot_layout() {
        let project_root = Path::new("/home/user/myproject");
        let branch = "feature/auth";

        let slot0 = compute_slot_worktree_path(project_root, branch, 0);
        assert_eq!(
            slot0,
            PathBuf::from("/home/user/myproject-worktrees/feature-auth"),
            "slot 0 should map to the standard worktree path"
        );

        let slot1 = compute_slot_worktree_path(project_root, branch, 1);
        let slot2 = compute_slot_worktree_path(project_root, branch, 2);
        assert_eq!(
            slot1,
            PathBuf::from("/home/user/myproject-worktrees/feature-auth-slot-1"),
            "slot 1 should be {{branch}}-slot-1"
        );
        assert_eq!(
            slot2,
            PathBuf::from("/home/user/myproject-worktrees/feature-auth-slot-2"),
            "slot 2 should be {{branch}}-slot-2"
        );
    }

    /// AC4: ephemeral branches named {branch}-slot-{N}
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn test_ensure_slot_worktrees_creates_ephemeral_branches_named_by_slot() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/ephemeral";
        let _slot0 = ensure_worktree(tmp.path(), branch, true, None).expect("slot 0");

        let paths = ensure_slot_worktrees(tmp.path(), branch, 3).expect("ensure_slot_worktrees");

        // Slot 0 stays on the base branch; slots 1+ use ephemeral branches.
        for slot in 1..3 {
            let wt_branch = get_current_branch(&paths[slot]).expect("get branch on slot worktree");
            let expected = format!("{}-slot-{}", branch, slot);
            assert_eq!(
                wt_branch, expected,
                "slot {} worktree should be on ephemeral branch {}",
                slot, expected
            );
        }
    }

    /// AC5: merge_slot_branches merges disjoint changes back into main branch
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn test_merge_slot_branches_merges_disjoint_changes() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/merge-back";
        let slot0 = ensure_worktree(tmp.path(), branch, true, None).expect("slot 0");

        let paths = ensure_slot_worktrees(tmp.path(), branch, 3).expect("ensure_slot_worktrees");

        // Make disjoint changes in each non-zero slot and commit them on the
        // ephemeral branch.
        for slot in 1..3 {
            let marker = paths[slot].join(format!("slot-{}-file.txt", slot));
            fs::write(&marker, format!("slot {} content", slot)).expect("write slot marker");
            Command::new("git")
                .args(["add", "."])
                .current_dir(&paths[slot])
                .output()
                .expect("git add in slot");
            Command::new("git")
                .args(["commit", "-m", &format!("slot-{} disjoint change", slot)])
                .current_dir(&paths[slot])
                .output()
                .expect("git commit in slot");
        }

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 3, &NoOpResolver, &paths);
        assert!(
            outcomes.failed_slots.is_empty(),
            "disjoint changes must merge cleanly: {:?}",
            outcomes.failed_slots
        );
        assert_eq!(
            outcomes.merged_slots,
            vec![1, 2],
            "slots 1 and 2 should both report merged"
        );

        // After merge, slot 0 (main branch worktree) must contain every
        // slot's file.
        for slot in 1..3 {
            let merged = slot0.join(format!("slot-{}-file.txt", slot));
            assert!(
                merged.exists(),
                "main branch worktree should contain slot {}'s file after merge: {}",
                slot,
                merged.display()
            );
        }
    }

    /// AC5b: merge_slot_branches recovers from partial failure.
    ///
    /// If slot 1 merges cleanly but slot 2's merge conflicts (e.g., out-of-band
    /// commit on the same path), `merge_slot_branches` must:
    ///   (a) abort the active merge so slot 0 returns to a clean working tree,
    ///   (b) record slot 2 in `failed_slots` with stderr captured,
    ///   (c) leave slot 0's HEAD pointing at the (already-merged) slot 1 commit,
    ///   (d) NOT propagate the failure as `Err` — the caller decides what to do.
    #[test]
    fn test_merge_slot_branches_recovers_from_partial_failure() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/partial-failure";
        let slot0 = ensure_worktree(tmp.path(), branch, true, None).expect("slot 0");

        let paths = ensure_slot_worktrees(tmp.path(), branch, 3).expect("ensure_slot_worktrees");

        // Slot 1: clean disjoint commit (will merge fine)
        fs::write(paths[1].join("slot-1-file.txt"), "slot 1 content").expect("write slot 1");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&paths[1])
            .output()
            .expect("git add slot 1");
        Command::new("git")
            .args(["commit", "-m", "slot-1 disjoint change"])
            .current_dir(&paths[1])
            .output()
            .expect("git commit slot 1");

        // Slot 2 + main both touch the SAME path with conflicting content,
        // forcing a merge conflict when slot-2's branch comes back.
        fs::write(paths[2].join("contended.txt"), "slot-2 version").expect("write slot 2");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&paths[2])
            .output()
            .expect("git add slot 2");
        Command::new("git")
            .args(["commit", "-m", "slot-2 contended change"])
            .current_dir(&paths[2])
            .output()
            .expect("git commit slot 2");

        // Out-of-band commit on the main branch via slot 0 worktree, touching
        // the same file with different content.
        fs::write(slot0.join("contended.txt"), "main version").expect("write main");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&slot0)
            .output()
            .expect("git add main");
        Command::new("git")
            .args(["commit", "-m", "main out-of-band change"])
            .current_dir(&slot0)
            .output()
            .expect("git commit main");

        let pre_merge_head = rev_parse(&slot0, "HEAD");

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 3, &NoOpResolver, &paths);

        assert_eq!(
            outcomes.merged_slots,
            vec![1],
            "slot 1 should merge cleanly; slot 2 should not"
        );
        assert_eq!(
            outcomes.failed_slots.len(),
            1,
            "exactly one slot should fail: {:?}",
            outcomes.failed_slots
        );
        assert_eq!(outcomes.failed_slots[0].0, 2, "failed slot must be slot 2");
        assert!(
            !outcomes.failed_slots[0].1.is_empty(),
            "failure should capture stderr context"
        );
        assert_eq!(
            outcomes.failed_slots[0].2,
            SlotFailureKind::ResolverAttempted,
            "NoOpResolver calls resolve() so kind must be ResolverAttempted"
        );

        // Slot 0 must NOT be in a half-merged state — `git merge --abort`
        // restored it to its pre-merge HEAD plus slot 1's clean merge.
        assert!(
            !slot0.join(".git/MERGE_HEAD").exists()
                && !slot0
                    .join(".git/worktrees")
                    .join("..")
                    .join("MERGE_HEAD")
                    .exists(),
            "no in-progress merge artifact should remain in slot 0"
        );

        // Slot 1 was already merged before slot 2 was attempted, so HEAD
        // should be one commit ahead of the pre-merge state.
        let post_merge_head = rev_parse(&slot0, "HEAD");
        assert_ne!(
            post_merge_head, pre_merge_head,
            "slot 1's clean merge should still be present"
        );

        // Slot 1's file is in slot 0; slot 2's contended file remains the
        // out-of-band content (not slot 2's version).
        assert!(
            slot0.join("slot-1-file.txt").exists(),
            "slot 1 file should be present after merge"
        );
        let contended = fs::read_to_string(slot0.join("contended.txt")).expect("read contended");
        assert_eq!(
            contended.trim(),
            "main version",
            "slot 2's failed merge must not have leaked into slot 0"
        );
    }

    /// Regression: merge-back must use the actual slot 0 path from `slot_paths`,
    /// not recompute it via `compute_slot_worktree_path`.
    ///
    /// When `project_root` is slot 0's worktree path (as happens when the loop
    /// runs from inside the matching worktree), `compute_slot_worktree_path`
    /// derives a sibling-of-slot0 directory that does not exist, causing ENOENT
    /// on the first `rev_parse_head` call. This test creates that exact topology
    /// and verifies the fix holds.
    #[test]
    fn test_merge_slot_branches_succeeds_when_invoked_from_inside_slot0_worktree() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/inside-slot0";

        // Set up slot worktrees from the main repo (tmp.path()). slot_paths[0]
        // is at compute_worktree_path(tmp.path(), branch) — a sibling directory,
        // NOT tmp.path() itself.
        let slot_paths =
            ensure_slot_worktrees(tmp.path(), branch, 2).expect("ensure_slot_worktrees");

        // Make a disjoint commit on slot 1 so the merge has work to do.
        fs::write(slot_paths[1].join("slot-1-file.txt"), "slot 1 content").expect("write slot 1");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&slot_paths[1])
            .output()
            .expect("git add slot 1");
        Command::new("git")
            .args(["commit", "-m", "slot-1 disjoint change"])
            .current_dir(&slot_paths[1])
            .output()
            .expect("git commit slot 1");

        // Pass slot_paths[0] as project_root — simulating the loop being
        // invoked from inside slot 0's worktree. The OLD code called
        // compute_slot_worktree_path(slot_paths[0], branch, 0), which computes
        // "{parent(slot_paths[0])}/{slot0_name}-worktrees/feat-inside-slot0"
        // — a path that does not exist — and failed with "rev-parse spawn:
        // No such file or directory". The NEW code uses slot_paths[0] directly.
        let outcomes = merge_slot_branches_with_resolver(
            &slot_paths[0],
            branch,
            2,
            &NoOpResolver,
            &slot_paths,
        );

        assert!(
            outcomes.failed_slots.is_empty(),
            "merge-back must not ENOENT when project_root is the slot-0 worktree path: {:?}",
            outcomes.failed_slots
        );
        assert_eq!(
            outcomes.merged_slots,
            vec![1],
            "slot 1 should merge cleanly into slot 0"
        );
        assert!(
            slot_paths[0].join("slot-1-file.txt").exists(),
            "slot 1's file must appear in slot 0 after merge"
        );
    }

    /// AC6: cleanup removes slot worktrees and deletes ephemeral branches.
    ///
    /// Slot 0 (the user's main branch worktree) MUST be preserved.
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn test_cleanup_slot_worktrees_removes_worktrees_and_branches() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/cleanup";
        let slot0 = ensure_worktree(tmp.path(), branch, true, None).expect("slot 0");

        let paths = ensure_slot_worktrees(tmp.path(), branch, 3).expect("ensure_slot_worktrees");

        // Sanity: slot 1 and 2 exist before cleanup
        for slot in 1..3 {
            assert!(
                paths[slot].exists(),
                "slot {} worktree should exist before cleanup",
                slot
            );
        }

        cleanup_slot_worktrees(tmp.path(), branch, 3).expect("cleanup_slot_worktrees");

        // Slot 0 preserved
        assert!(
            slot0.exists(),
            "slot 0 (main branch worktree) must be preserved across cleanup"
        );

        // Slots 1+ worktrees removed
        for slot in 1..3 {
            assert!(
                !paths[slot].exists(),
                "slot {} worktree should be removed after cleanup: {}",
                slot,
                paths[slot].display()
            );
        }

        // Ephemeral branches deleted
        for slot in 1..3 {
            let ephemeral = format!("refs/heads/{}-slot-{}", branch, slot);
            let verify = Command::new("git")
                .args(["rev-parse", "--verify", &ephemeral])
                .current_dir(tmp.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git rev-parse");
            assert!(
                !verify.success(),
                "ephemeral branch {} should be deleted after cleanup",
                ephemeral
            );
        }
    }

    /// AC7: Verify git's exclusivity constraint — the reason ephemeral branches
    /// are required. This test does NOT depend on the new slot functions;
    /// it asserts git's own behavior.
    ///
    /// Known-bad discriminator: if we ever tried to reuse the same branch for
    /// two worktrees, this would silently succeed and parallel slots would race
    /// on the same ref. The failure mode we protect against is "branch already
    /// checked out".
    #[test]
    fn test_git_worktree_add_same_branch_fails() {
        let tmp = setup_git_repo_with_file();
        let branch = "feat/exclusive";

        // First worktree on the branch (uses our helper so it's routed through
        // the normal creation path).
        let first = ensure_worktree(tmp.path(), branch, true, None).expect("create first worktree");
        assert!(first.exists(), "first worktree should exist");

        // Try to add a second worktree checking out the SAME branch at a
        // different path — git must refuse.
        let second_path = tmp.path().join("second-wt");
        let output = Command::new("git")
            .args(["worktree", "add", second_path.to_str().unwrap(), branch])
            .current_dir(tmp.path())
            .output()
            .expect("invoke git worktree add");

        assert!(
            !output.status.success(),
            "git worktree add with a branch already checked out must fail; \
             stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        assert!(
            stderr.contains("already") || stderr.contains("checked out"),
            "error should mention branch already checked out, got: {}",
            stderr
        );

        // Confirm the second path was not created as a worktree.
        assert!(
            !second_path.join(".git").exists(),
            "second worktree path should not have a .git entry after failure"
        );

        // And git's worktree list still shows only the original two
        // (main repo + first worktree) — no stale entry for second_path.
        let list_out = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .expect("git worktree list");
        let list_str = String::from_utf8_lossy(&list_out.stdout);
        assert!(
            !list_str.contains(second_path.to_str().unwrap()),
            "failed worktree must not appear in git worktree list, got: {}",
            list_str
        );

        // Sanity: branch head unchanged — failure left no side effects on the ref.
        let head_before = rev_parse(tmp.path(), &format!("refs/heads/{}", branch));
        assert!(
            !head_before.is_empty(),
            "branch ref should still resolve after failed worktree add"
        );
    }

    #[test]
    fn test_list_other_roots_excludes_self() {
        let tmp = setup_git_repo_with_file();

        let wt =
            ensure_worktree(tmp.path(), "feature/other-root", true, None).expect("create worktree");

        let others = list_other_roots(tmp.path());
        assert_eq!(others.len(), 1, "should find exactly one other root");
        assert_eq!(
            others[0].canonicalize().unwrap(),
            wt.canonicalize().unwrap(),
        );

        // From the worktree's perspective, main repo is the "other"
        let others_from_wt = list_other_roots(&wt);
        assert_eq!(others_from_wt.len(), 1);
        assert_eq!(
            others_from_wt[0].canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap(),
        );
    }

    // --- FEAT-001: resolver-callback seam tests ---
    //
    // Set up a single conflicting slot (slot 1) on top of an out-of-band main
    // commit, then exercise each MergeResolverOutcome variant via a mock
    // resolver. The mock's `resolve` runs real git commands in slot 0 so the
    // outer MERGE_HEAD/HEAD inspection sees authentic state, not a stub.

    /// Mock resolver that runs an arbitrary closure inside slot 0 and returns
    /// a configured `MergeResolverOutcome`. Captures the context it received
    /// for assertion purposes.
    struct MockResolver<F: Fn(&Path)> {
        outcome: MergeResolverOutcome,
        side_effect: F,
        invocations: std::cell::RefCell<u32>,
        last_conflicted: std::cell::RefCell<Vec<String>>,
        last_branch: std::cell::RefCell<String>,
    }

    impl<F: Fn(&Path)> MockResolver<F> {
        fn new(outcome: MergeResolverOutcome, side_effect: F) -> Self {
            Self {
                outcome,
                side_effect,
                invocations: std::cell::RefCell::new(0),
                last_conflicted: std::cell::RefCell::new(Vec::new()),
                last_branch: std::cell::RefCell::new(String::new()),
            }
        }
    }

    impl<F: Fn(&Path)> MergeResolver for MockResolver<F> {
        fn resolve(&self, ctx: ResolverContext<'_>) -> MergeResolverOutcome {
            *self.invocations.borrow_mut() += 1;
            *self.last_conflicted.borrow_mut() = ctx.conflicted_files.to_vec();
            *self.last_branch.borrow_mut() = ctx.ephemeral_branch.to_string();
            (self.side_effect)(ctx.slot0_path);
            self.outcome.clone()
        }
    }

    /// Build a 2-slot repo where slot 1's commit conflicts with an
    /// out-of-band commit on the main branch via slot 0. Returns
    /// `(tmp, slot0_path, pre_merge_head)`.
    fn setup_conflicting_slot1(branch: &str) -> (tempfile::TempDir, PathBuf, String) {
        let tmp = setup_git_repo_with_file();
        let slot0 = ensure_worktree(tmp.path(), branch, true, None).expect("slot 0");
        let paths = ensure_slot_worktrees(tmp.path(), branch, 2).expect("ensure_slot_worktrees");

        // Slot 1: conflicting commit on contended.txt
        fs::write(paths[1].join("contended.txt"), "slot-1 version").expect("write slot 1");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&paths[1])
            .output()
            .expect("git add slot 1");
        Command::new("git")
            .args(["commit", "-m", "slot-1 contended change"])
            .current_dir(&paths[1])
            .output()
            .expect("git commit slot 1");

        // Out-of-band conflicting commit on main via slot 0
        fs::write(slot0.join("contended.txt"), "main version").expect("write main");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&slot0)
            .output()
            .expect("git add main");
        Command::new("git")
            .args(["commit", "-m", "main out-of-band change"])
            .current_dir(&slot0)
            .output()
            .expect("git commit main");

        let pre = rev_parse(&slot0, "HEAD");
        (tmp, slot0, pre)
    }

    /// AC7: resolver returns Resolved + creates a real commit → slot is
    /// merged, MERGE_HEAD cleared, HEAD advanced.
    #[test]
    fn test_resolver_resolved_with_real_commit_lands_in_merged_slots() {
        let branch = "feat/resolver-resolved";
        let (tmp, slot0, pre_merge_head) = setup_conflicting_slot1(branch);
        let slot_paths = vec![
            compute_slot_worktree_path(tmp.path(), branch, 0),
            compute_slot_worktree_path(tmp.path(), branch, 1),
        ];

        let resolver = MockResolver::new(MergeResolverOutcome::Resolved, |slot0_path| {
            // Pretend to resolve by overwriting the file and committing.
            // `git commit -am` inside an in-progress merge finalizes it.
            fs::write(slot0_path.join("contended.txt"), "resolved version").expect("write");
            let out = Command::new("git")
                .args(["commit", "-am", "resolve conflict"])
                .current_dir(slot0_path)
                .output()
                .expect("git commit");
            assert!(
                out.status.success(),
                "merge-commit should succeed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        });

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 2, &resolver, &slot_paths);

        assert_eq!(*resolver.invocations.borrow(), 1, "resolver called once");
        assert!(
            !resolver.last_conflicted.borrow().is_empty(),
            "resolver received the conflicted-file list: {:?}",
            resolver.last_conflicted.borrow()
        );
        assert_eq!(
            resolver.last_branch.borrow().as_str(),
            &format!("{}-slot-1", branch),
            "resolver received the ephemeral branch name"
        );
        assert_eq!(
            outcomes.merged_slots,
            vec![1],
            "slot 1 should land in merged_slots: {:?}",
            outcomes
        );
        assert!(
            outcomes.failed_slots.is_empty(),
            "no slot should be failed: {:?}",
            outcomes.failed_slots
        );

        let post = rev_parse(&slot0, "HEAD");
        assert_ne!(post, pre_merge_head, "HEAD must advance past pre-merge");
        assert!(
            !slot0.join(".git/MERGE_HEAD").exists(),
            "MERGE_HEAD must be cleared"
        );
    }

    /// AC8: resolver returns Aborted + runs real `git merge --abort` → slot
    /// in failed_slots with 'declined' diagnostic, HEAD == pre_merge_head.
    #[test]
    fn test_resolver_aborted_lands_in_failed_slots_with_declined() {
        let branch = "feat/resolver-aborted";
        let (tmp, slot0, pre_merge_head) = setup_conflicting_slot1(branch);
        let slot_paths = vec![
            compute_slot_worktree_path(tmp.path(), branch, 0),
            compute_slot_worktree_path(tmp.path(), branch, 1),
        ];

        let resolver = MockResolver::new(MergeResolverOutcome::Aborted, |slot0_path| {
            let out = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(slot0_path)
                .output()
                .expect("git merge --abort");
            assert!(out.status.success(), "merge --abort should succeed");
        });

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 2, &resolver, &slot_paths);

        assert!(
            outcomes.merged_slots.is_empty(),
            "no slot should be merged: {:?}",
            outcomes.merged_slots
        );
        assert_eq!(outcomes.failed_slots.len(), 1);
        let (slot, diag, kind) = &outcomes.failed_slots[0];
        assert_eq!(*slot, 1);
        assert!(
            diag.to_lowercase().contains("declined"),
            "diagnostic should mention 'declined': {}",
            diag
        );
        assert_eq!(*kind, SlotFailureKind::ResolverAttempted);
        assert_eq!(
            rev_parse(&slot0, "HEAD"),
            pre_merge_head,
            "HEAD must remain at pre_merge_head"
        );
        assert!(
            !slot0.join(".git/MERGE_HEAD").exists(),
            "MERGE_HEAD must not remain set"
        );
    }

    /// AC9: resolver returns Failed("timed out") → slot in failed_slots,
    /// HEAD reset to pre_merge_head, diagnostic contains 'timed out'.
    #[test]
    fn test_resolver_failed_resets_head_and_records_message() {
        let branch = "feat/resolver-failed";
        let (tmp, slot0, pre_merge_head) = setup_conflicting_slot1(branch);
        let slot_paths = vec![
            compute_slot_worktree_path(tmp.path(), branch, 0),
            compute_slot_worktree_path(tmp.path(), branch, 1),
        ];

        // Resolver does nothing (simulating a hard error before any cleanup).
        let resolver = MockResolver::new(MergeResolverOutcome::Failed("timed out".into()), |_| {});

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 2, &resolver, &slot_paths);

        assert!(outcomes.merged_slots.is_empty());
        assert_eq!(outcomes.failed_slots.len(), 1);
        let (slot, diag, kind) = &outcomes.failed_slots[0];
        assert_eq!(*slot, 1);
        assert!(
            diag.contains("timed out"),
            "diagnostic should contain 'timed out': {}",
            diag
        );
        assert_eq!(*kind, SlotFailureKind::ResolverAttempted);
        assert_eq!(
            rev_parse(&slot0, "HEAD"),
            pre_merge_head,
            "HEAD must be reset to pre_merge_head"
        );
        assert!(
            !slot0.join(".git/MERGE_HEAD").exists(),
            "MERGE_HEAD must be cleared by the reset"
        );
    }

    /// AC10 + known-bad: resolver returns Resolved but does NOTHING (no
    /// commit, MERGE_HEAD untouched) → slot in failed_slots, HEAD reset,
    /// diagnostic mentions MERGE_HEAD. Catches naive impls that trust the
    /// resolver's return value.
    #[test]
    fn test_resolver_lying_resolved_is_downgraded_to_failed() {
        let branch = "feat/resolver-lying";
        let (tmp, slot0, pre_merge_head) = setup_conflicting_slot1(branch);
        let slot_paths = vec![
            compute_slot_worktree_path(tmp.path(), branch, 0),
            compute_slot_worktree_path(tmp.path(), branch, 1),
        ];

        // Lying resolver: claims Resolved but does NOT commit. MERGE_HEAD
        // should still be set after this no-op runs.
        let resolver = MockResolver::new(MergeResolverOutcome::Resolved, |_| {});

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 2, &resolver, &slot_paths);

        assert!(
            outcomes.merged_slots.is_empty(),
            "lying resolver must NOT push to merged_slots: {:?}",
            outcomes.merged_slots
        );
        assert_eq!(outcomes.failed_slots.len(), 1);
        let (slot, diag, kind) = &outcomes.failed_slots[0];
        assert_eq!(*slot, 1);
        assert!(
            diag.contains("MERGE_HEAD"),
            "diagnostic should mention MERGE_HEAD: {}",
            diag
        );
        assert_eq!(*kind, SlotFailureKind::ResolverAttempted);
        assert_eq!(
            rev_parse(&slot0, "HEAD"),
            pre_merge_head,
            "HEAD must be reset to pre_merge_head"
        );
        assert!(
            !slot0.join(".git/MERGE_HEAD").exists(),
            "MERGE_HEAD must be cleared by the forced reset"
        );
    }

    /// AC6: resolver clears MERGE_HEAD without advancing HEAD (degenerate
    /// case — silent abort dressed up as Resolved) → slot in failed_slots
    /// with 'resolution did not advance HEAD'.
    #[test]
    fn test_resolver_resolved_without_head_advance_is_failed() {
        let branch = "feat/resolver-no-advance";
        let (tmp, slot0, pre_merge_head) = setup_conflicting_slot1(branch);
        let slot_paths = vec![
            compute_slot_worktree_path(tmp.path(), branch, 0),
            compute_slot_worktree_path(tmp.path(), branch, 1),
        ];

        let resolver = MockResolver::new(MergeResolverOutcome::Resolved, |slot0_path| {
            // Clear MERGE_HEAD without committing — this is what `git merge
            // --abort` does, but the resolver is lying about Resolved.
            let out = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(slot0_path)
                .output()
                .expect("git merge --abort");
            assert!(out.status.success());
        });

        let outcomes =
            merge_slot_branches_with_resolver(tmp.path(), branch, 2, &resolver, &slot_paths);

        assert!(outcomes.merged_slots.is_empty());
        assert_eq!(outcomes.failed_slots.len(), 1);
        let (_, diag, kind) = &outcomes.failed_slots[0];
        assert!(
            diag.contains("did not advance HEAD"),
            "diagnostic should mention HEAD non-advance: {}",
            diag
        );
        assert_eq!(*kind, SlotFailureKind::ResolverAttempted);
        assert_eq!(rev_parse(&slot0, "HEAD"), pre_merge_head);
    }

    /// SlotFailureKind::PreResolver is set when rev_parse_head fails before
    /// the resolver is ever invoked (slot 0 worktree does not exist).
    #[test]
    fn test_slot_failure_kind_pre_resolver_on_missing_worktree() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        // slot_paths[0] points to tmp.path() (exists but is not a git repo),
        // so rev_parse_head fails before the resolver is ever invoked.
        let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().join("slot-1")];
        // NoOpResolver must not be called — slot fails at rev_parse_head.
        let outcomes = merge_slot_branches_with_resolver(
            tmp.path(),
            "test-branch",
            2,
            &NoOpResolver,
            &slot_paths,
        );
        assert_eq!(outcomes.failed_slots.len(), 1);
        let (_, _, kind) = &outcomes.failed_slots[0];
        assert_eq!(
            *kind,
            SlotFailureKind::PreResolver,
            "rev-parse failure before resolver invocation must be PreResolver kind"
        );
    }

    /// Helpers exist as pub(crate) and behave correctly.
    #[test]
    fn test_helpers_list_conflicted_and_has_unresolved_merge() {
        let branch = "feat/resolver-helpers";
        let (tmp, slot0, _pre) = setup_conflicting_slot1(branch);

        // Trigger the conflict directly so we can inspect helpers without
        // engaging the resolver path.
        let ephemeral = format!("{}-slot-1", branch);
        let merge = Command::new("git")
            .args(["merge", "--no-edit", &ephemeral])
            .current_dir(&slot0)
            .output()
            .expect("git merge");
        assert!(!merge.status.success(), "merge should conflict");

        // has_unresolved_merge should return Ok(true) mid-merge.
        assert_eq!(has_unresolved_merge(&slot0), Ok(true));

        // list_conflicted_files should mention contended.txt.
        let conflicted = list_conflicted_files(&slot0).expect("list_conflicted_files");
        assert!(
            conflicted.iter().any(|f| f == "contended.txt"),
            "conflicted list should contain contended.txt: {:?}",
            conflicted
        );

        // rev_parse_head should return a non-empty SHA.
        let head = rev_parse_head(&slot0).expect("rev_parse_head");
        assert!(!head.is_empty());

        // Cleanup so other tests / drop don't see a stuck merge.
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&slot0)
            .output();
        assert_eq!(has_unresolved_merge(&slot0), Ok(false));

        // Keep `tmp` alive until after we've read from slot0.
        drop(tmp);
    }

    /// MergeResolverOutcome has exactly the three variants Resolved, Aborted,
    /// Failed(String) — compile-time check that nothing else exists.
    #[test]
    fn test_merge_resolver_outcome_has_three_variants() {
        // Exhaustive match — adding a variant breaks this test.
        fn classify(o: MergeResolverOutcome) -> &'static str {
            match o {
                MergeResolverOutcome::Resolved => "resolved",
                MergeResolverOutcome::Aborted => "aborted",
                MergeResolverOutcome::Failed(_) => "failed",
            }
        }
        assert_eq!(classify(MergeResolverOutcome::Resolved), "resolved");
        assert_eq!(classify(MergeResolverOutcome::Aborted), "aborted");
        assert_eq!(classify(MergeResolverOutcome::Failed("x".into())), "failed");
    }

    #[test]
    fn test_merged_attributes_inserts_block_into_empty_file() {
        let result = merged_attributes_contents("").expect("must rewrite empty file");
        assert!(result.contains(ATTR_MARKER_BEGIN));
        assert!(result.contains(ATTR_MARKER_END));
        assert!(result.contains("tasks/progress*.txt merge=union"));
        assert!(result.contains(".task-mgr/tasks/progress*.txt merge=union"));
    }

    #[test]
    fn test_merged_attributes_appends_to_existing_user_content() {
        let existing = "*.bin binary\n";
        let result = merged_attributes_contents(existing).expect("must rewrite");
        // User content preserved at the top.
        assert!(result.starts_with("*.bin binary\n"));
        // Managed block follows.
        assert!(result.contains(ATTR_MARKER_BEGIN));
        assert!(result.contains("tasks/progress*.txt merge=union"));
    }

    #[test]
    fn test_merged_attributes_idempotent_when_block_matches() {
        let initial = merged_attributes_contents("").expect("first write");
        // Second pass over the result of the first must be a no-op.
        assert!(
            merged_attributes_contents(&initial).is_none(),
            "expected no rewrite when block already matches"
        );
    }

    #[test]
    fn test_merged_attributes_rewrites_drifted_block() {
        // Simulate a stale block from a prior task-mgr version with a different body.
        let stale = format!(
            "{}\nold-pattern.txt merge=union\n{}\n",
            ATTR_MARKER_BEGIN, ATTR_MARKER_END
        );
        let result = merged_attributes_contents(&stale).expect("must rewrite drifted block");
        assert!(!result.contains("old-pattern.txt"));
        assert!(result.contains("tasks/progress*.txt merge=union"));
    }

    #[test]
    fn test_merged_attributes_preserves_user_lines_after_block() {
        let stale = format!(
            "*.bin binary\n{}\nold.txt merge=union\n{}\n*.lock -text\n",
            ATTR_MARKER_BEGIN, ATTR_MARKER_END
        );
        let result = merged_attributes_contents(&stale).expect("must rewrite");
        assert!(result.starts_with("*.bin binary\n"));
        assert!(result.contains("*.lock -text"));
        assert!(!result.contains("old.txt"));
        assert!(result.contains("tasks/progress*.txt merge=union"));
    }

    #[test]
    fn test_ensure_progress_union_merge_writes_attributes_file() {
        let tmp = setup_git_repo_with_file();
        ensure_progress_union_merge(tmp.path()).expect("ensure");
        let attrs = tmp.path().join(".git/info/attributes");
        let body = std::fs::read_to_string(&attrs).expect("read attributes");
        assert!(body.contains("tasks/progress*.txt merge=union"));
        assert!(body.contains(".task-mgr/tasks/progress*.txt merge=union"));
    }

    #[test]
    fn test_ensure_progress_union_merge_is_idempotent_on_disk() {
        let tmp = setup_git_repo_with_file();
        ensure_progress_union_merge(tmp.path()).expect("first");
        let first = std::fs::read_to_string(tmp.path().join(".git/info/attributes")).unwrap();
        ensure_progress_union_merge(tmp.path()).expect("second");
        let second = std::fs::read_to_string(tmp.path().join(".git/info/attributes")).unwrap();
        assert_eq!(first, second, "second call must not modify file");
    }

    #[test]
    fn test_progress_union_merge_resolves_concurrent_appends_without_conflict() {
        // End-to-end: confirm git's union driver actually fires for the
        // configured pattern when two branches each append distinct lines.
        let tmp = setup_git_repo_with_file();
        let repo = tmp.path();

        std::fs::create_dir_all(repo.join("tasks")).unwrap();
        let progress_path = repo.join("tasks/progress-test.txt");
        std::fs::write(&progress_path, "line-base\n").unwrap();
        Command::new("git")
            .args(["add", "tasks/progress-test.txt"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base progress"])
            .current_dir(repo)
            .output()
            .unwrap();

        ensure_progress_union_merge(repo).expect("attrs");

        // Branch A appends one line.
        Command::new("git")
            .args(["checkout", "-b", "branch-a"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(&progress_path, "line-base\nline-a\n").unwrap();
        Command::new("git")
            .args(["commit", "-am", "a"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Branch B (from main) appends a different line.
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "-b", "branch-b"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(&progress_path, "line-base\nline-b\n").unwrap();
        Command::new("git")
            .args(["commit", "-am", "b"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Merge A into B — without union, this would conflict on the trailing line.
        let merge = Command::new("git")
            .args(["merge", "--no-edit", "branch-a"])
            .current_dir(repo)
            .output()
            .expect("git merge");
        assert!(
            merge.status.success(),
            "merge should succeed via union driver: stderr={}, stdout={}",
            String::from_utf8_lossy(&merge.stderr),
            String::from_utf8_lossy(&merge.stdout),
        );

        let merged_body = fs::read_to_string(&progress_path).unwrap();
        assert!(merged_body.contains("line-a"));
        assert!(merged_body.contains("line-b"));
    }

    // ===== reconcile_stale_ephemeral_slots tests (FEAT-005) =====

    /// Returns true iff `branch` exists in `project_root`.
    fn branch_exists(project_root: &Path, branch: &str) -> bool {
        let out = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
            .current_dir(project_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git rev-parse spawn");
        out.success()
    }

    /// Set up a 2-slot worktree topology on `branch`. Returns
    /// `(tmp, project_root, slot_paths)`. `slot_paths[1]` is the slot 1
    /// ephemeral worktree path under test.
    fn setup_ephemeral_slot_for_recon(branch: &str) -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
        let tmp = setup_git_repo_with_file();
        let project_root = tmp.path().to_path_buf();
        let paths = ensure_slot_worktrees(&project_root, branch, 2)
            .expect("ensure_slot_worktrees in test setup");
        (tmp, project_root, paths)
    }

    /// Make a single commit on the given slot worktree by writing a sentinel
    /// file. Used to push the ephemeral past the base branch's tip.
    fn commit_on_slot(slot_path: &Path, fname: &str, msg: &str) {
        fs::write(slot_path.join(fname), "slot-content").expect("write slot file");
        let add = Command::new("git")
            .args(["add", "."])
            .current_dir(slot_path)
            .output()
            .expect("git add");
        assert!(add.status.success());
        let commit = Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(slot_path)
            .output()
            .expect("git commit");
        assert!(
            commit.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_deletes_orphan_branch() {
        let branch = "feat/recon-orphan";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let slot1_path = slot_paths[1].clone();
        let ephemeral = ephemeral_slot_branch(branch, 1);

        // Forcibly remove the slot 1 worktree directory (simulating an
        // out-of-band deletion / abrupt loss). The branch ref remains.
        let _ = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                slot1_path.to_str().unwrap(),
            ])
            .current_dir(&project_root)
            .output();
        // Belt-and-braces: ensure dir is really gone.
        let _ = std::fs::remove_dir_all(&slot1_path);
        assert!(!slot1_path.exists(), "slot 1 dir should be gone");
        assert!(
            branch_exists(&project_root, &ephemeral),
            "ephemeral branch should still exist before reconcile"
        );

        reconcile_stale_ephemeral_slots(&project_root, branch, 0)
            .expect("reconcile should succeed for orphan-branch case");

        assert!(
            !branch_exists(&project_root, &ephemeral),
            "orphan ephemeral branch should be deleted"
        );
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_deletes_clean_merged() {
        let branch = "feat/recon-merged";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let slot1_path = slot_paths[1].clone();
        let ephemeral = ephemeral_slot_branch(branch, 1);

        // Slot 1 was forked from `branch` and has no extra commits → its
        // tip is identical to `branch`'s tip → ancestor check returns true.
        // Topology stress: advance the base branch past the ephemeral so a
        // tree-diff implementation would falsely report "un-merged". The
        // ancestor primitive must still classify this as merged.
        commit_on_slot(&slot_paths[0], "advance.txt", "advance base past ephemeral");

        assert!(slot1_path.exists());
        assert!(branch_exists(&project_root, &ephemeral));

        reconcile_stale_ephemeral_slots(&project_root, branch, 0)
            .expect("reconcile should succeed for clean-merged case");

        assert!(
            !branch_exists(&project_root, &ephemeral),
            "merged ephemeral branch should be deleted"
        );
        assert!(
            !slot1_path.exists(),
            "merged ephemeral worktree should be removed"
        );
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_aborts_on_unmerged_when_halt_enabled() {
        let branch = "feat/recon-unmerged-halt";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let slot1_path = slot_paths[1].clone();
        let ephemeral = ephemeral_slot_branch(branch, 1);

        // Push slot 1 past base with a commit so it is no longer an ancestor.
        commit_on_slot(&slot1_path, "slot1-only.txt", "slot 1 only commit");

        let result = reconcile_stale_ephemeral_slots(&project_root, branch, 2);
        assert!(
            result.is_err(),
            "reconcile must abort when halt_threshold > 0 and un-merged ephemeral exists: {:?}",
            result
        );

        // Branch and worktree must be preserved on abort.
        assert!(
            branch_exists(&project_root, &ephemeral),
            "ephemeral branch must be preserved on abort"
        );
        assert!(
            slot1_path.exists(),
            "ephemeral worktree must be preserved on abort"
        );
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_warns_on_unmerged_when_halt_disabled() {
        let branch = "feat/recon-unmerged-warn";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let slot1_path = slot_paths[1].clone();
        let ephemeral = ephemeral_slot_branch(branch, 1);

        commit_on_slot(&slot1_path, "slot1-only.txt", "slot 1 only commit");

        // halt_threshold = 0 → legacy permissive: warn but proceed.
        reconcile_stale_ephemeral_slots(&project_root, branch, 0)
            .expect("reconcile should return Ok when halt_threshold == 0");

        // Critical AC: must NOT delete in case 3 even with halt disabled.
        assert!(
            branch_exists(&project_root, &ephemeral),
            "un-merged ephemeral branch must be preserved (never auto-deleted)"
        );
        assert!(
            slot1_path.exists(),
            "un-merged ephemeral worktree must be preserved"
        );
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_aborts_on_dirty_worktree() {
        let branch = "feat/recon-dirty";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let slot1_path = slot_paths[1].clone();
        let ephemeral = ephemeral_slot_branch(branch, 1);

        // Introduce uncommitted change in slot 1 worktree.
        fs::write(slot1_path.join("dirty.txt"), "uncommitted").expect("write dirty file");

        // Dirty must abort regardless of halt_threshold.
        for halt in [0u32, 2u32] {
            let result = reconcile_stale_ephemeral_slots(&project_root, branch, halt);
            assert!(
                result.is_err(),
                "dirty worktree must abort regardless of halt_threshold (halt={}): {:?}",
                halt,
                result
            );
            assert!(
                branch_exists(&project_root, &ephemeral),
                "ephemeral branch must be preserved on dirty-abort (halt={})",
                halt
            );
            assert!(
                slot1_path.exists(),
                "ephemeral worktree must be preserved on dirty-abort (halt={})",
                halt
            );
        }
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_no_branches_returns_ok() {
        let tmp = setup_git_repo_with_file();
        let project_root = tmp.path().to_path_buf();

        // No slot branches exist; pattern matches nothing → early Ok.
        reconcile_stale_ephemeral_slots(&project_root, "feat/no-such-branch", 2)
            .expect("empty branch list should produce Ok with no work");
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_skips_malformed_branch_and_continues() {
        let branch = "feat/recon-malformed";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let slot1_path = slot_paths[1].clone();
        let ephemeral = ephemeral_slot_branch(branch, 1);

        // Inject a malformed sibling matching the glob but with a non-numeric
        // suffix. classify_ephemeral_branch returns Err for this; the
        // orchestrator logs and proceeds to clean the legitimate slot 1.
        let malformed = format!("{}-slot-foo", branch);
        let out = Command::new("git")
            .args(["branch", &malformed, branch])
            .current_dir(&project_root)
            .output()
            .expect("create malformed branch");
        assert!(out.status.success());

        // Slot 1 is at base's tip → clean-merged → should be cleaned up.
        // Advance base past the ephemeral so the test exercises the
        // ancestor-vs-tree-diff distinction at the same time.
        commit_on_slot(&slot_paths[0], "advance.txt", "advance base");

        reconcile_stale_ephemeral_slots(&project_root, branch, 0)
            .expect("reconcile should not abort because of malformed sibling");

        assert!(
            !branch_exists(&project_root, &ephemeral),
            "legitimate clean-merged ephemeral should still be cleaned up despite malformed sibling"
        );
        assert!(
            !slot1_path.exists(),
            "legitimate clean-merged worktree should be removed"
        );
        // The malformed branch is left in place — operator-actionable.
        assert!(
            branch_exists(&project_root, &malformed),
            "malformed sibling branch is preserved (logged + skipped, never auto-deleted)"
        );
    }

    #[test]
    fn test_reconcile_stale_ephemeral_slots_is_idempotent() {
        let branch = "feat/recon-idempotent";
        let (_tmp, project_root, slot_paths) = setup_ephemeral_slot_for_recon(branch);
        let _ = slot_paths;

        reconcile_stale_ephemeral_slots(&project_root, branch, 0).expect("first pass");
        // Second pass: nothing left matching the pattern → fast Ok.
        reconcile_stale_ephemeral_slots(&project_root, branch, 0)
            .expect("second pass should be a no-op Ok");
    }
}
