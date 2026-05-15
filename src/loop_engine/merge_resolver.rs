//! Claude-driven `MergeResolver` for parallel-slot merge-back conflicts.
//!
//! When `merge_slot_branches_with_resolver` (FEAT-001) hits a non-zero
//! `git merge --no-edit` exit, this resolver spawns Claude in slot 0's
//! already-conflicted worktree, hands it the conflict context, and lets it
//! commit the resolution (or `git merge --abort`). The merge function then
//! re-validates by inspecting MERGE_HEAD/HEAD, so a dishonest "Resolved" is
//! caught and downgraded by the caller — this module's contract is only:
//! pick the right post-spawn outcome based on observable git state.
//!
//! Wired by FEAT-003 at the engine wave-merge call site.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::loop_engine::claude::{SpawnOpts, spawn_claude};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::watchdog::TimeoutConfig;
use crate::loop_engine::worktree::{
    MergeResolver, MergeResolverOutcome, ResolverContext, has_unresolved_merge, rev_parse_head,
};

/// Soft budget (chars) the prompt suggests Claude stay under for narration so
/// the stream tee remains readable when many slots conflict in one wave. Not
/// enforced — Claude is allowed to exceed it; the orchestrator never truncates.
const RESPONSE_CHAR_BUDGET_HINT: usize = 4000;

/// Tools Claude must never invoke from inside a merge-resolver spawn. The
/// list is enforced via `--disallowedTools` so even prompt-injection through
/// adversarial commit messages on the ephemeral branch (which the prompt
/// directs Claude to read) cannot trigger destructive history-rewriting or
/// network-publishing operations. Listed prefix-style; anything starting with
/// the matched form is denied. Intentionally does NOT include `git reset` —
/// the resolver is allowed to use `git merge --abort` (which uses a reset
/// internally), and a blanket reset prohibition would break that path. The
/// prompt's textual prohibition handles the residual `reset --hard <other>`
/// risk; combined with `working_dir` scoping that is the defense-in-depth
/// posture.
const RESOLVER_DISALLOWED_TOOLS: &str = "Bash(git push:*),\
Bash(git push --force:*),\
Bash(git push --force-with-lease:*),\
Bash(git branch -D:*),\
Bash(git branch -d:*),\
Bash(git rebase:*),\
Bash(git filter-branch:*),\
Bash(git reflog expire:*),\
Bash(git update-ref:*),\
Bash(git commit --amend:*)";

/// Coarse summary of a `spawn_claude` call, used only by `infer_outcome` so
/// the inference logic stays a pure function (no `ClaudeResult`/IO types).
///
/// `Success` / `NonZero` / `TimedOut` come from a successfully-spawned
/// process; `SpawnErr` represents a `spawn_claude` `Err(...)` (binary
/// missing, ENOENT, etc.) where no inspection was possible.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SpawnSummary {
    Success,
    NonZero(i32),
    TimedOut(u64),
    SpawnErr(String),
}

/// Inspect the worktree after Claude has exited and return `(merge_head_present,
/// head_changed)`. Fails with a descriptive message if either git probe errors.
fn probe_post_spawn(slot0_path: &Path, pre_merge_head: &str) -> Result<(bool, bool), String> {
    let merge_head_present = has_unresolved_merge(slot0_path)
        .map_err(|e| format!("post-resolve MERGE_HEAD probe failed: {}", e))?;
    let head_changed = rev_parse_head(slot0_path)
        .map_err(|e| format!("post-resolve rev-parse failed: {}", e))
        .map(|h| h != pre_merge_head)?;
    Ok((merge_head_present, head_changed))
}

/// Map post-spawn git state to a `MergeResolverOutcome`.
///
/// Pure: takes only the booleans + spawn summary, returns the outcome. The
/// caller in `merge_slot_branches_with_resolver` already re-validates a
/// returned `Resolved`, so this function trusts observable state when both
/// the merge state AND the spawn agree (MERGE_HEAD absent + HEAD advanced +
/// success exit). A non-zero/timed-out exit is treated as Failed regardless
/// of state — losing the crash signal would mask a genuinely broken run that
/// happened to land in a clean-looking tree.
///
/// Decision table:
///
/// | spawn        | merge_head | head_changed | outcome                                |
/// |--------------|------------|--------------|----------------------------------------|
/// | SpawnErr(e)  | *          | *            | Failed("spawn error: e")               |
/// | *            | true       | *            | Failed(reason from spawn)              |
/// | NonZero(c)   | false      | *            | Failed("non-zero exit: c (state=…)")   |
/// | TimedOut(s)  | false      | *            | Failed("timed out after Xs (state=…)") |
/// | Success      | false      | true         | Resolved                               |
/// | Success      | false      | false        | Aborted                                |
fn infer_outcome(
    merge_head_present: bool,
    head_changed: bool,
    spawn_result: &SpawnSummary,
) -> MergeResolverOutcome {
    if let SpawnSummary::SpawnErr(msg) = spawn_result {
        return MergeResolverOutcome::Failed(format!("spawn error: {}", msg));
    }
    if merge_head_present {
        let reason = match spawn_result {
            SpawnSummary::TimedOut(secs) => format!("Claude timed out after {}s", secs),
            SpawnSummary::NonZero(code) => format!("Claude exited with code {}", code),
            SpawnSummary::Success => {
                "incomplete: MERGE_HEAD still set after Claude exited cleanly".to_string()
            }
            SpawnSummary::SpawnErr(_) => unreachable!("handled above"),
        };
        return MergeResolverOutcome::Failed(reason);
    }
    // MERGE_HEAD absent — but a non-zero or timed-out exit still means the
    // resolver run was unhealthy. Don't silently downgrade a crash to Aborted
    // just because the tree looks clean (Claude could have aborted then
    // panicked, or never aborted but exited late after a previous abort).
    let state = if head_changed {
        "HEAD advanced"
    } else {
        "HEAD unchanged"
    };
    match spawn_result {
        SpawnSummary::TimedOut(secs) => {
            MergeResolverOutcome::Failed(format!("Claude timed out after {}s ({})", secs, state))
        }
        SpawnSummary::NonZero(code) => {
            MergeResolverOutcome::Failed(format!("Claude exited with code {} ({})", code, state))
        }
        SpawnSummary::Success if head_changed => MergeResolverOutcome::Resolved,
        SpawnSummary::Success => MergeResolverOutcome::Aborted,
        SpawnSummary::SpawnErr(_) => unreachable!("handled at top"),
    }
}

/// Build the prompt handed to Claude in slot 0's conflicted worktree.
///
/// The prompt scopes the work to the listed files only (no broad refactors),
/// names the ephemeral branch being merged in, and spells out the two valid
/// exits — `git commit --no-edit` after resolving every marker, or
/// `git merge --abort` if it judges the conflict unresolvable. Explicit
/// prohibitions for `git push`, branch deletion, and resets outside the
/// merge keep Claude from writing to shared state if it goes off-script.
///
/// Pure / no IO. Tested by literal substring assertion.
fn build_resolver_prompt(
    slot: usize,
    ephemeral_branch: &str,
    conflicted_files: &[String],
) -> String {
    let files_block = if conflicted_files.is_empty() {
        // Defensive — `resolve` short-circuits on empty input before this
        // function is called, but keep the rendering robust for callers
        // who hit the prompt builder directly (e.g. tests).
        "(none — caller should have short-circuited)".to_string()
    } else {
        conflicted_files
            .iter()
            .map(|f| format!("  - {}", f))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "You are resolving a git merge conflict in this worktree (parallel-loop slot {slot}).\n\
\n\
Context:\n\
  - The branch `{ephemeral_branch}` was merged into the current branch and produced conflicts.\n\
  - The repository is in mid-merge state: MERGE_HEAD is set, the index has unresolved entries, \
and the conflicted files contain `<<<<<<<` / `=======` / `>>>>>>>` markers.\n\
  - Your working directory is already the conflicted worktree. Do not `cd` elsewhere.\n\
\n\
Conflicted files (resolve ONLY these — do not touch any other file):\n\
{files_block}\n\
\n\
What to do:\n\
  1. For each file above, open it, locate every conflict block delimited by `<<<<<<<`, \
`=======`, and `>>>>>>>`, and replace the block with the correct merged content. Remove all \
three marker lines along with the rejected side.\n\
  2. If the same logical change appears on both sides (e.g. two formatting tweaks), keep one. \
If the changes are independent (e.g. two new fields), keep both. If they genuinely conflict on \
semantic logic — both sides changed the same expression to do different things — run \
`git merge --abort` and stop (step 4). Do not guess at intent; the orchestrator will surface \
the unresolved conflict for human review.\n\
  3. After every file is conflict-marker-free, run `git add` on each resolved path and then \
`git commit --no-edit` to finish the merge. Do NOT amend, rewrite, or rebase any existing \
commits.\n\
  4. If you judge the conflict too risky to resolve correctly (e.g. semantic logic conflict \
you can't reason about, or you cannot tell which side's intent is correct), run \
`git merge --abort` and stop. Do not partially commit.\n\
\n\
Strict prohibitions — do NOT under any circumstance:\n\
  - run `git push` (any flavor: push, push --force, push --force-with-lease)\n\
  - delete any branch (`git branch -d`, `git branch -D`)\n\
  - run `git reset --hard` against any commit OUTSIDE the current merge (resetting to the \
pre-merge HEAD via `git merge --abort` is fine; resetting to a different commit is NOT)\n\
  - touch any file not listed above\n\
  - run `git rebase`, `git filter-branch`, or rewrite history in any other form\n\
  - modify `.git/`, `.task-mgr/`, or any tasks/*.json file\n\
\n\
Response budget: keep your narration under {cap} characters (soft target — the orchestrator \
does not truncate). The orchestrator captures git state directly after you exit, so you do not \
need to summarize the resolution — just do the work and exit.\n",
        slot = slot,
        ephemeral_branch = ephemeral_branch,
        files_block = files_block,
        cap = RESPONSE_CHAR_BUDGET_HINT,
    )
}

/// `MergeResolver` that spawns Claude in slot 0's conflicted worktree.
///
/// Wired by FEAT-003 at the engine wave-merge call site. Holds borrowed
/// references to the loop's signal flag and DB dir for the duration of a
/// single wave's merge-back, plus the resolved Claude model and a
/// configurable per-conflict timeout.
pub(crate) struct ClaudeMergeResolver<'a> {
    /// Loop's resolved default Claude model — passed straight through to
    /// `--model`. Owned `String` so the resolver outlives any per-call
    /// borrow site.
    pub model: String,
    /// `TASK_MGR_DIR` to pin for the spawned subprocess. None disables.
    pub db_dir: Option<&'a Path>,
    /// Loop's shared signal flag so SIGINT/SIGTERM kills the resolver too.
    pub signal_flag: Option<&'a SignalFlag>,
    /// Hard cap on a single merge-resolution Claude run.
    pub claude_timeout: Duration,
    /// `--effort` value handed to Claude. Defaults the engine to "medium";
    /// projects with frequent semantic conflicts can configure "high".
    pub effort: String,
}

impl<'a> ClaudeMergeResolver<'a> {
    /// Build the prompt, construct spawn options, call `spawn_claude`, and map
    /// the result to a `SpawnSummary`. No git IO; pure subprocess orchestration.
    fn run_resolver_spawn(&self, ctx: &ResolverContext<'_>) -> SpawnSummary {
        let prompt = build_resolver_prompt(ctx.slot, ctx.ephemeral_branch, ctx.conflicted_files);
        let timeout = TimeoutConfig {
            base_timeout: self.claude_timeout,
            initial_extension: Duration::from_secs(0),
            last_activity_epoch: Arc::new(AtomicU64::new(0)),
        };
        let permission_mode = PermissionMode::Auto {
            allowed_tools: None,
        };
        match spawn_claude(
            &prompt,
            &permission_mode,
            SpawnOpts {
                signal_flag: self.signal_flag,
                working_dir: Some(ctx.slot0_path),
                model: Some(self.model.as_str()),
                effort: Some(self.effort.as_str()),
                timeout: Some(timeout),
                db_dir: self.db_dir,
                disallowed_tools: Some(RESOLVER_DISALLOWED_TOOLS),
                cleanup_title_artifact: true,
                ..SpawnOpts::default()
            },
        ) {
            Ok(result) if result.timed_out => SpawnSummary::TimedOut(self.claude_timeout.as_secs()),
            Ok(result) if result.exit_code == 0 => SpawnSummary::Success,
            Ok(result) => SpawnSummary::NonZero(result.exit_code),
            Err(e) => SpawnSummary::SpawnErr(e.to_string()),
        }
    }
}

impl<'a> MergeResolver for ClaudeMergeResolver<'a> {
    fn resolve(&self, ctx: ResolverContext<'_>) -> MergeResolverOutcome {
        // Defensive short-circuit: no conflicts means nothing for Claude to
        // act on; a spawn here would let it freelance an unrelated edit.
        if ctx.conflicted_files.is_empty() {
            return MergeResolverOutcome::Failed(
                "no conflicts reported, refusing to spawn (likely dirty WT blocked merge precondition; preflight should have prevented this — see worktree::prepare_slot0_for_merge; if this fires, check for non-gitignored dirty files in slot 0)".to_string(),
            );
        }
        let spawn_summary = self.run_resolver_spawn(&ctx);
        // SpawnErr: worktree is unchanged past the original failed merge, so
        // MERGE_HEAD is definitionally still set. Skip git probes.
        if let SpawnSummary::SpawnErr(_) = &spawn_summary {
            return infer_outcome(true, false, &spawn_summary);
        }
        match probe_post_spawn(ctx.slot0_path, ctx.pre_merge_head) {
            Ok((merge_head_present, head_changed)) => {
                infer_outcome(merge_head_present, head_changed, &spawn_summary)
            }
            Err(e) => MergeResolverOutcome::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_resolver_prompt ---

    #[test]
    fn prompt_contains_branch_and_each_file() {
        let prompt = build_resolver_prompt(
            1,
            "feat-x-slot-1",
            &["src/foo.rs".into(), "src/bar.rs".into()],
        );
        assert!(prompt.contains("feat-x-slot-1"), "missing branch name");
        assert!(prompt.contains("src/foo.rs"), "missing src/foo.rs");
        assert!(prompt.contains("src/bar.rs"), "missing src/bar.rs");
        assert!(
            prompt.contains("git merge --abort"),
            "missing abort instruction"
        );
        assert!(prompt.contains("git commit"), "missing commit instruction");
        assert!(prompt.contains("<<<<<<<"), "missing conflict marker");
    }

    #[test]
    fn prompt_contains_explicit_prohibitions() {
        let prompt = build_resolver_prompt(2, "feat-y-slot-2", &["src/a.rs".into()]);
        assert!(prompt.contains("git push"), "must prohibit git push");
        assert!(
            prompt.contains("branch -d"),
            "must prohibit branch deletion"
        );
        assert!(prompt.contains("reset --hard"), "must prohibit hard reset");
        assert!(
            prompt.contains("git rebase"),
            "must prohibit history rewrite"
        );
        assert!(
            prompt.contains("filter-branch"),
            "must prohibit filter-branch"
        );
    }

    #[test]
    fn disallowed_tools_blocks_destructive_git_operations() {
        // The constant feeds directly into spawn_claude's --disallowedTools.
        // If any of these regress, the prompt-injection defense lapses.
        for forbidden in [
            "Bash(git push:*)",
            "Bash(git push --force:*)",
            "Bash(git push --force-with-lease:*)",
            "Bash(git branch -D:*)",
            "Bash(git branch -d:*)",
            "Bash(git rebase:*)",
            "Bash(git filter-branch:*)",
            "Bash(git update-ref:*)",
            "Bash(git commit --amend:*)",
        ] {
            assert!(
                RESOLVER_DISALLOWED_TOOLS.contains(forbidden),
                "missing tool block: {}",
                forbidden,
            );
        }
    }

    #[test]
    fn prompt_includes_marker_triplet() {
        let prompt = build_resolver_prompt(1, "any-branch", &["x.rs".into()]);
        // All three marker types per spec (d/c instructions).
        assert!(prompt.contains("<<<<<<<"));
        assert!(prompt.contains("======="));
        assert!(prompt.contains(">>>>>>>"));
    }

    #[test]
    fn prompt_includes_response_cap() {
        let prompt = build_resolver_prompt(1, "b", &["f.rs".into()]);
        assert!(
            prompt.contains(&RESPONSE_CHAR_BUDGET_HINT.to_string()),
            "must reference the char cap so Claude knows the budget"
        );
    }

    #[test]
    fn prompt_with_many_files_lists_all_of_them() {
        let files: Vec<String> = (0..5).map(|i| format!("src/mod_{}.rs", i)).collect();
        let prompt = build_resolver_prompt(3, "feat-z-slot-3", &files);
        for f in &files {
            assert!(prompt.contains(f), "missing {}", f);
        }
    }

    // --- infer_outcome ---

    #[test]
    fn infer_resolved_when_merge_head_absent_and_head_advanced() {
        let outcome = infer_outcome(false, true, &SpawnSummary::Success);
        assert_eq!(outcome, MergeResolverOutcome::Resolved);
    }

    #[test]
    fn infer_aborted_when_merge_head_absent_and_head_unchanged() {
        let outcome = infer_outcome(false, false, &SpawnSummary::Success);
        assert_eq!(outcome, MergeResolverOutcome::Aborted);
    }

    #[test]
    fn infer_failed_when_merge_head_present_with_clean_exit() {
        let outcome = infer_outcome(true, false, &SpawnSummary::Success);
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("incomplete"), "got: {}", msg);
                assert!(msg.contains("MERGE_HEAD"), "got: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_when_merge_head_present_with_timeout() {
        let outcome = infer_outcome(true, false, &SpawnSummary::TimedOut(600));
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("timed out"), "got: {}", msg);
                assert!(msg.contains("600"), "must include duration: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_when_merge_head_present_with_nonzero_exit() {
        let outcome = infer_outcome(true, true, &SpawnSummary::NonZero(2));
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("code 2"), "got: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_when_clean_state_but_nonzero_exit() {
        // S1 fix: a non-zero exit must not be silently downgraded to Aborted
        // just because MERGE_HEAD is absent. Could be a Claude crash post-abort.
        let outcome = infer_outcome(false, false, &SpawnSummary::NonZero(2));
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("code 2"), "got: {}", msg);
                assert!(msg.contains("HEAD unchanged"), "must report state: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_when_clean_state_but_timed_out() {
        // S1 fix: same as above for timeouts. Watchdog-killed runs must not
        // pretend to be Aborted.
        let outcome = infer_outcome(false, true, &SpawnSummary::TimedOut(600));
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("timed out"), "got: {}", msg);
                assert!(msg.contains("600"), "got: {}", msg);
                assert!(msg.contains("HEAD advanced"), "must report state: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_propagates_spawn_err() {
        let outcome = infer_outcome(
            false,
            true,
            &SpawnSummary::SpawnErr("ENOENT: claude binary not found".into()),
        );
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("spawn error"), "got: {}", msg);
                assert!(
                    msg.contains("ENOENT"),
                    "must include underlying error: {}",
                    msg
                );
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    // --- ClaudeMergeResolver short-circuit ---

    /// AC: empty `conflicted_files` returns Failed without spawning Claude.
    /// Uses `CLAUDE_BINARY=/nonexistent/binary` so that IF the resolver
    /// failed to short-circuit, the spawn would error with a different
    /// message. The exact-substring assertion distinguishes the two paths.
    #[test]
    fn empty_conflicted_files_short_circuits_failed_without_spawn() {
        let signal_flag = SignalFlag::new();
        let resolver = ClaudeMergeResolver {
            model: "test-model".to_string(),
            db_dir: None,
            signal_flag: Some(&signal_flag),
            claude_timeout: Duration::from_secs(60),
            effort: "medium".to_string(),
        };
        // Note: we deliberately pass a path that doesn't exist; the
        // short-circuit must trigger before any git/claude call would
        // touch it.
        let outcome = resolver.resolve(ResolverContext {
            slot: 1,
            slot0_path: Path::new("/this/path/does/not/exist/xyzzy"),
            ephemeral_branch: "any-branch",
            conflicted_files: &[],
            pre_merge_head: "deadbeef",
        });
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(
                    msg.starts_with("no conflicts reported, refusing to spawn"),
                    "diagnostic should start with the no-conflicts prefix: {}",
                    msg
                );
                assert!(
                    msg.contains("preflight should have prevented this"),
                    "diagnostic should mention the preflight pointer: {}",
                    msg
                );
            }
            other => panic!("expected Failed(no conflicts...), got {:?}", other),
        }
    }
}
