//! Provider-agnostic `MergeResolver` for parallel-slot merge-back conflicts.
//!
//! When `merge_slot_branches_with_resolver` (FEAT-001) hits a non-zero
//! `git merge --no-edit` exit, this resolver spawns the configured primary
//! LLM provider in slot 0's already-conflicted worktree, hands it the conflict
//! context, and lets it commit the resolution (or `git merge --abort`). On any
//! `Failed` outcome, an optional one-shot fallback to
//! `models.providers.<primary>.fallback` is attempted. The merge function then
//! re-validates by inspecting MERGE_HEAD/HEAD, so a dishonest "Resolved" is
//! caught and downgraded by the caller — this module's contract is only:
//! pick the right post-spawn outcome based on observable git state.
//!
//! Wired at the engine wave-merge call site and startup auto-recovery.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::model::{MergeResolverPlan, Provider, ResolvedModelsConfig};
use crate::loop_engine::project_config::ProjectConfig;
use crate::loop_engine::protected_state;
use crate::loop_engine::runner::{self, RunnerCapability, RunnerOpts, runner_kind_for};
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::watchdog::TimeoutConfig;
use crate::loop_engine::worktree::{
    MergeResolver, MergeResolverOutcome, ResolverContext, has_unresolved_merge, rev_parse_head,
};
use crate::output::ui;

/// Soft budget (chars) the prompt suggests the agent stay under for narration so
/// the stream tee remains readable when many slots conflict in one wave. Not
/// enforced — the agent is allowed to exceed it; the orchestrator never truncates.
const RESPONSE_CHAR_BUDGET_HINT: usize = 4000;

/// Tools runners that support `DisallowedTools` must never invoke from inside
/// a merge-resolver spawn. Enforced via `--disallowedTools` /
/// `--disallowed-tools` so even prompt-injection through adversarial commit
/// messages on the ephemeral branch cannot trigger destructive history-rewriting
/// or network-publishing operations. Listed prefix-style; anything starting with
/// the matched form is denied. Intentionally does NOT include `git reset` —
/// the resolver is allowed to use `git merge --abort` (which uses a reset
/// internally), and a blanket reset prohibition would break that path. The
/// prompt's textual prohibition handles the residual `reset --hard <other>`
/// risk; combined with `working_dir` scoping that is the defense-in-depth
/// posture. Codex does not support this flag — it relies on the prompt +
/// `protected_state` snapshot instead.
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

/// Coarse summary of a runner spawn, used only by `infer_outcome` so
/// the inference logic stays a pure function (no runner/IO types).
///
/// `Success` / `NonZero` / `TimedOut` come from a successfully-spawned
/// process; `SpawnErr` represents a dispatch `Err(...)` (binary missing,
/// auth failure, capability reject, ENOENT, etc.) where no inspection was
/// possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnSummary {
    Success,
    NonZero(i32),
    TimedOut(u64),
    SpawnErr(String),
}

/// Inspect the worktree after the agent has exited and return `(merge_head_present,
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
/// Pure: takes only the booleans + spawn summary + provider label, returns the
/// outcome. The caller in `merge_slot_branches_with_resolver` already re-validates
/// a returned `Resolved`, so this function trusts observable state when both
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
    provider_label: &str,
) -> MergeResolverOutcome {
    if let SpawnSummary::SpawnErr(msg) = spawn_result {
        return MergeResolverOutcome::Failed(format!("spawn error: {}", msg));
    }
    if merge_head_present {
        let reason = match spawn_result {
            SpawnSummary::TimedOut(secs) => {
                format!("{} timed out after {}s", provider_label, secs)
            }
            SpawnSummary::NonZero(code) => {
                format!("{} exited with code {}", provider_label, code)
            }
            SpawnSummary::Success => format!(
                "incomplete: MERGE_HEAD still set after {} exited cleanly",
                provider_label
            ),
            SpawnSummary::SpawnErr(_) => unreachable!("handled above"),
        };
        return MergeResolverOutcome::Failed(reason);
    }
    // MERGE_HEAD absent — but a non-zero or timed-out exit still means the
    // resolver run was unhealthy. Don't silently downgrade a crash to Aborted
    // just because the tree looks clean (agent could have aborted then
    // panicked, or never aborted but exited late after a previous abort).
    let state = if head_changed {
        "HEAD advanced"
    } else {
        "HEAD unchanged"
    };
    match spawn_result {
        SpawnSummary::TimedOut(secs) => MergeResolverOutcome::Failed(format!(
            "{} timed out after {}s ({})",
            provider_label, secs, state
        )),
        SpawnSummary::NonZero(code) => MergeResolverOutcome::Failed(format!(
            "{} exited with code {} ({})",
            provider_label, code, state
        )),
        SpawnSummary::Success if head_changed => MergeResolverOutcome::Resolved,
        SpawnSummary::Success => MergeResolverOutcome::Aborted,
        SpawnSummary::SpawnErr(_) => unreachable!("handled at top"),
    }
}

/// Build the prompt handed to the coding agent in slot 0's conflicted worktree.
///
/// The prompt scopes the work to the listed files only (no broad refactors),
/// names the ephemeral branch being merged in, and spells out the two valid
/// exits — `git commit --no-edit` after resolving every marker, or
/// `git merge --abort` if it judges the conflict unresolvable. Explicit
/// prohibitions for `git push`, branch deletion, and resets outside the
/// merge keep the agent from writing to shared state if it goes off-script.
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

/// Spawn override used by unit tests to avoid real CLI binaries. Production
/// always leaves this `None` and routes through `runner::dispatch`.
pub(crate) type MergeSpawnOverride =
    Arc<dyn Fn(Provider, Option<&str>) -> SpawnSummary + Send + Sync>;

/// `MergeResolver` that dispatches to the configured primary (and optional
/// fallback) LLM provider in slot 0's conflicted worktree.
///
/// Wired at the engine wave-merge call site and startup auto-recovery. Holds
/// borrowed references to the loop's signal flag and DB/tasks dirs for the
/// duration of a single wave's merge-back, plus the resolved provider plan and
/// a configurable per-conflict timeout.
pub(crate) struct LlmMergeResolver<'a> {
    pub primary_provider: Provider,
    pub primary_model: Option<String>,
    pub fallback_provider: Option<Provider>,
    pub fallback_model: Option<String>,
    /// `TASK_MGR_DIR` to pin for the spawned subprocess. None disables.
    pub db_dir: Option<&'a Path>,
    /// Tasks directory for Codex `protected_state` snapshots. None skips the
    /// guard even for Codex (tests / callers without a tasks dir).
    pub tasks_dir: Option<&'a Path>,
    /// Loop's shared signal flag so SIGINT/SIGTERM kills the resolver too.
    pub signal_flag: Option<&'a SignalFlag>,
    /// Hard cap on a single merge-resolution provider run.
    pub timeout: Duration,
    /// Effort value handed to the runner when it supports Effort. Defaults the
    /// engine to "medium"; projects with frequent semantic conflicts can
    /// configure "high".
    pub effort: String,
    /// When set, called instead of `runner::dispatch`. Production always
    /// leaves this `None`.
    pub spawn_override: Option<MergeSpawnOverride>,
}

impl<'a> LlmMergeResolver<'a> {
    /// Build from a [`MergeResolverPlan`] + project knobs. Shared by live waves
    /// and startup auto-recovery so plan resolution cannot drift.
    pub fn from_config(
        resolved: &ResolvedModelsConfig,
        project_config: &ProjectConfig,
        signal_flag: Option<&'a SignalFlag>,
        db_dir: Option<&'a Path>,
        tasks_dir: Option<&'a Path>,
    ) -> Self {
        let plan = crate::loop_engine::model::merge_resolver_plan(resolved);
        Self::from_plan(
            &plan,
            project_config
                .merge_resolver_effort
                .clone()
                .unwrap_or_else(|| "medium".to_string()),
            Duration::from_secs(project_config.merge_resolver_timeout_secs.unwrap_or(600)),
            signal_flag,
            db_dir,
            tasks_dir,
        )
    }

    /// Build from an already-resolved plan (tests / callers that own the plan).
    pub fn from_plan(
        plan: &MergeResolverPlan<'_>,
        effort: String,
        timeout: Duration,
        signal_flag: Option<&'a SignalFlag>,
        db_dir: Option<&'a Path>,
        tasks_dir: Option<&'a Path>,
    ) -> Self {
        Self {
            primary_provider: plan.primary.provider,
            primary_model: plan.primary.model.map(str::to_string),
            fallback_provider: plan.fallback.map(|f| f.provider),
            fallback_model: plan.fallback.and_then(|f| f.model.map(str::to_string)),
            db_dir,
            tasks_dir,
            signal_flag,
            timeout,
            effort,
            spawn_override: None,
        }
    }

    /// Dispatch (or override) for one provider attempt and map to `SpawnSummary`.
    fn run_resolver_spawn(
        &self,
        provider: Provider,
        model: Option<&str>,
        ctx: &ResolverContext<'_>,
    ) -> SpawnSummary {
        if let Some(ref override_fn) = self.spawn_override {
            return override_fn(provider, model);
        }

        let prompt = build_resolver_prompt(ctx.slot, ctx.ephemeral_branch, ctx.conflicted_files);
        let kind = runner_kind_for(provider);
        let timeout = TimeoutConfig {
            base_timeout: self.timeout,
            initial_extension: Duration::from_secs(0),
            last_activity_epoch: Arc::new(AtomicU64::new(0)),
        };
        let permission_mode = PermissionMode::Auto {
            allowed_tools: None,
        };
        let effort = if kind.supports(RunnerCapability::Effort) {
            Some(self.effort.as_str())
        } else {
            None
        };
        let disallowed_tools = if kind.supports(RunnerCapability::DisallowedTools) {
            Some(RESOLVER_DISALLOWED_TOOLS)
        } else {
            None
        };

        // Codex (and any future state-guarded runner): snapshot orchestrator-
        // owned files before the agent can rewrite them.
        let protected_snapshot = match (self.db_dir, self.tasks_dir) {
            (Some(db), Some(tasks)) => protected_state::Snapshot::take(db, tasks, kind),
            _ => None,
        };

        let spawn_result = runner::dispatch(
            kind,
            &prompt,
            &permission_mode,
            RunnerOpts {
                signal_flag: self.signal_flag,
                working_dir: Some(ctx.slot0_path),
                model,
                timeout: Some(timeout),
                stream_json: false,
                effort,
                disallowed_tools,
                db_dir: self.db_dir,
                use_pty: false,
                ..RunnerOpts::default()
            },
        );

        if let Some(ref snap) = protected_snapshot {
            // Best-effort restore; merge resolution continues either way.
            // Fatal SQLite corruption is logged but does not change the spawn
            // summary — the merge caller's HEAD re-validation is authoritative
            // for whether the slot is considered resolved.
            let _ = protected_state::apply_verify_outcome(snap, "merge-resolver");
        }

        match spawn_result {
            Ok(result) if result.timed_out => SpawnSummary::TimedOut(self.timeout.as_secs()),
            Ok(result) if result.exit_code == 0 => SpawnSummary::Success,
            Ok(result) => SpawnSummary::NonZero(result.exit_code),
            Err(e) => SpawnSummary::SpawnErr(e.to_string()),
        }
    }

    /// One full resolve attempt for a single provider: spawn + probe + infer.
    fn resolve_once(
        &self,
        provider: Provider,
        model: Option<&str>,
        ctx: &ResolverContext<'_>,
    ) -> MergeResolverOutcome {
        let label = provider.as_str();
        let spawn_summary = self.run_resolver_spawn(provider, model, ctx);
        // SpawnErr: worktree is unchanged past the original failed merge, so
        // MERGE_HEAD is definitionally still set. Skip git probes.
        if let SpawnSummary::SpawnErr(_) = &spawn_summary {
            return infer_outcome(true, false, &spawn_summary, label);
        }
        match probe_post_spawn(ctx.slot0_path, ctx.pre_merge_head) {
            Ok((merge_head_present, head_changed)) => {
                infer_outcome(merge_head_present, head_changed, &spawn_summary, label)
            }
            Err(e) => MergeResolverOutcome::Failed(e),
        }
    }
}

impl<'a> MergeResolver for LlmMergeResolver<'a> {
    fn resolve(&self, ctx: ResolverContext<'_>) -> MergeResolverOutcome {
        // Defensive short-circuit: no conflicts means nothing for the agent to
        // act on; a spawn here would let it freelance an unrelated edit.
        if ctx.conflicted_files.is_empty() {
            return MergeResolverOutcome::Failed(
                "no conflicts reported, refusing to spawn (likely dirty WT blocked merge precondition; preflight should have prevented this — see worktree::prepare_slot0_for_merge; if this fires, check for non-gitignored dirty files in slot 0)".to_string(),
            );
        }

        let primary_outcome =
            self.resolve_once(self.primary_provider, self.primary_model.as_deref(), &ctx);
        match &primary_outcome {
            MergeResolverOutcome::Resolved | MergeResolverOutcome::Aborted => primary_outcome,
            MergeResolverOutcome::Failed(primary_msg) => {
                let Some(fb_provider) = self.fallback_provider else {
                    return primary_outcome;
                };
                // Only retry when the conflict is still live. If primary left
                // MERGE_HEAD cleared (e.g. aborted then crashed), re-spawning
                // is pointless / dangerous.
                let still_conflicted = has_unresolved_merge(ctx.slot0_path).unwrap_or(true);
                if !still_conflicted {
                    return primary_outcome;
                }
                ui::emit(&format!(
                    "merge resolver: primary {} failed ({}); retrying with fallback {}",
                    self.primary_provider.as_str(),
                    primary_msg,
                    fb_provider.as_str()
                ));
                self.resolve_once(fb_provider, self.fallback_model.as_deref(), &ctx)
            }
        }
    }
}

// Re-export SpawnSummary constructors for the override type in tests via a
// thin public test helper module pattern — tests in this file use the private
// enum directly.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        // The constant feeds into runners that support DisallowedTools.
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
        assert!(prompt.contains("<<<<<<<"));
        assert!(prompt.contains("======="));
        assert!(prompt.contains(">>>>>>>"));
    }

    #[test]
    fn prompt_includes_response_cap() {
        let prompt = build_resolver_prompt(1, "b", &["f.rs".into()]);
        assert!(
            prompt.contains(&RESPONSE_CHAR_BUDGET_HINT.to_string()),
            "must reference the char cap so the agent knows the budget"
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
        let outcome = infer_outcome(false, true, &SpawnSummary::Success, "claude");
        assert_eq!(outcome, MergeResolverOutcome::Resolved);
    }

    #[test]
    fn infer_aborted_when_merge_head_absent_and_head_unchanged() {
        let outcome = infer_outcome(false, false, &SpawnSummary::Success, "claude");
        assert_eq!(outcome, MergeResolverOutcome::Aborted);
    }

    #[test]
    fn infer_failed_when_merge_head_present_with_clean_exit() {
        let outcome = infer_outcome(true, false, &SpawnSummary::Success, "grok");
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("incomplete"), "got: {}", msg);
                assert!(msg.contains("MERGE_HEAD"), "got: {}", msg);
                assert!(msg.contains("grok"), "must name provider: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_when_merge_head_present_with_timeout() {
        let outcome = infer_outcome(true, false, &SpawnSummary::TimedOut(600), "codex");
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("timed out"), "got: {}", msg);
                assert!(msg.contains("600"), "must include duration: {}", msg);
                assert!(msg.contains("codex"), "must name provider: {}", msg);
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn infer_failed_when_merge_head_present_with_nonzero_exit() {
        let outcome = infer_outcome(true, true, &SpawnSummary::NonZero(2), "claude");
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
        // just because MERGE_HEAD is absent. Could be a crash post-abort.
        let outcome = infer_outcome(false, false, &SpawnSummary::NonZero(2), "claude");
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
        let outcome = infer_outcome(false, true, &SpawnSummary::TimedOut(600), "claude");
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
            &SpawnSummary::SpawnErr("ENOENT: binary not found".into()),
            "grok",
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

    // --- LlmMergeResolver short-circuit + fallback ---

    fn base_resolver<'a>(
        signal_flag: &'a SignalFlag,
        spawn_override: Option<MergeSpawnOverride>,
    ) -> LlmMergeResolver<'a> {
        LlmMergeResolver {
            primary_provider: Provider::Grok,
            primary_model: Some("grok-build".into()),
            fallback_provider: Some(Provider::Claude),
            fallback_model: Some("claude-opus-5".into()),
            db_dir: None,
            tasks_dir: None,
            signal_flag: Some(signal_flag),
            timeout: Duration::from_secs(60),
            effort: "medium".to_string(),
            spawn_override,
        }
    }

    /// AC: empty `conflicted_files` returns Failed without spawning.
    #[test]
    fn empty_conflicted_files_short_circuits_failed_without_spawn() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = Arc::clone(&calls);
        let signal_flag = SignalFlag::new();
        let resolver = base_resolver(
            &signal_flag,
            Some(Arc::new(move |_p, _m| {
                calls_c.fetch_add(1, Ordering::SeqCst);
                SpawnSummary::Success
            })),
        );
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
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "must not spawn when conflicted_files is empty"
        );
    }

    /// Primary SpawnErr + configured fallback: fallback is invoked.
    /// Uses a non-existent path so probe_post_spawn on fallback Success
    /// path fails the probe → Failed (still proves fallback was called).
    #[test]
    fn fallback_invoked_when_primary_spawn_fails() {
        let calls: Arc<std::sync::Mutex<Vec<Provider>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let calls_c = Arc::clone(&calls);
        let signal_flag = SignalFlag::new();
        let resolver = base_resolver(
            &signal_flag,
            Some(Arc::new(move |p, _m| {
                calls_c.lock().unwrap().push(p);
                match p {
                    Provider::Grok => SpawnSummary::SpawnErr("ENOENT: grok missing".into()),
                    Provider::Claude => SpawnSummary::SpawnErr("ENOENT: claude missing".into()),
                    other => SpawnSummary::SpawnErr(format!("unexpected {other:?}")),
                }
            })),
        );
        let outcome = resolver.resolve(ResolverContext {
            slot: 1,
            // Path does not need to exist for SpawnErr (skips probes).
            slot0_path: Path::new("/this/path/does/not/exist/xyzzy"),
            ephemeral_branch: "feat-x-slot-1",
            conflicted_files: &["src/foo.rs".into()],
            pre_merge_head: "deadbeef",
        });
        match outcome {
            MergeResolverOutcome::Failed(msg) => {
                assert!(msg.contains("spawn error"), "got: {}", msg);
            }
            other => panic!("expected Failed after both spawns fail, got {:?}", other),
        }
        let seen = calls.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![Provider::Grok, Provider::Claude],
            "primary then fallback must both run on primary SpawnErr"
        );
    }

    /// Aborted (success + head unchanged) must NOT fire fallback.
    /// We simulate that without git by overriding spawn to Success and
    /// making probe return head-unchanged — but probe needs real git.
    /// Instead: unit-test the match arm logic by verifying a Success
    /// SpawnErr path that... actually for Aborted we need MERGE_HEAD
    /// absent. Skip full Aborted-no-fallback integration here; the pure
    /// `infer_outcome` Aborted case is covered, and resolve only falls
    /// through on Failed.
    #[test]
    fn no_fallback_when_fallback_not_configured() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = Arc::clone(&calls);
        let signal_flag = SignalFlag::new();
        let mut resolver = base_resolver(
            &signal_flag,
            Some(Arc::new(move |_p, _m| {
                calls_c.fetch_add(1, Ordering::SeqCst);
                SpawnSummary::SpawnErr("boom".into())
            })),
        );
        resolver.fallback_provider = None;
        resolver.fallback_model = None;
        let outcome = resolver.resolve(ResolverContext {
            slot: 1,
            slot0_path: Path::new("/this/path/does/not/exist/xyzzy"),
            ephemeral_branch: "feat-x-slot-1",
            conflicted_files: &["src/foo.rs".into()],
            pre_merge_head: "deadbeef",
        });
        assert!(matches!(outcome, MergeResolverOutcome::Failed(_)));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "without fallback only primary may spawn"
        );
    }

    #[test]
    fn from_plan_copies_primary_and_fallback() {
        let plan = MergeResolverPlan {
            primary: crate::loop_engine::model::AuxiliaryLlmPlan {
                provider: Provider::Grok,
                model: Some("grok-build"),
            },
            fallback: Some(crate::loop_engine::model::AuxiliaryLlmPlan {
                provider: Provider::Claude,
                model: Some("claude-opus-5"),
            }),
        };
        let r = LlmMergeResolver::from_plan(
            &plan,
            "high".into(),
            Duration::from_secs(120),
            None,
            None,
            None,
        );
        assert_eq!(r.primary_provider, Provider::Grok);
        assert_eq!(r.primary_model.as_deref(), Some("grok-build"));
        assert_eq!(r.fallback_provider, Some(Provider::Claude));
        assert_eq!(r.fallback_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(r.effort, "high");
        assert_eq!(r.timeout, Duration::from_secs(120));
    }
}
