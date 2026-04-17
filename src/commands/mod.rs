//! CLI command implementations for task-mgr.
//!
//! This module contains the implementation of all CLI commands:
//! - `init` - Initialize database from JSON PRD file(s)
//! - `list` - List tasks with optional filtering
//! - `show` - Show detailed task information
//! - `next` - Get the next recommended task
//! - `complete` - Mark tasks as completed
//! - `fail` - Mark tasks as blocked/skipped/irrelevant
//! - `skip` - Skip a task intentionally
//! - `irrelevant` - Mark task as no longer needed
//! - `run` - Run lifecycle management
//! - `export` - Export database to JSON
//! - `doctor` - Health check and repair
//! - `learn` - Record learnings
//! - `recall` - Query learnings
//! - `learnings` - List learnings
//! - `stats` - Show progress summary
//! - `history` - Show run history
//! - `review` - Review blocked/skipped tasks
//! - `migrate` - Database schema migrations
//! - `import-learnings` - Import learnings from JSON
//! - `apply-learning` - Record that a learning was applied

pub mod add;
pub mod apply_learning;
pub mod complete;
pub mod curate;
pub mod decisions;
pub mod dependency_checker;
pub mod doctor;
pub mod export;
pub mod fail;
pub mod history;
pub mod import_learnings;
pub mod init;
pub mod invalidate_learning;
pub mod irrelevant;
pub mod learn;
pub mod learnings;
pub mod list;
pub mod migrate;
pub mod models;
pub mod next;
pub mod recall;
pub mod reset;
pub mod review;
pub mod run;
pub mod show;
pub mod skip;
pub mod stats;
pub mod unblock;
pub mod worktrees;

pub use add::{AddResult, AddTaskInput, PrioritySource, add, format_text as format_add_text};
pub use apply_learning::{
    ApplyLearningResult, apply_learning, format_text as format_apply_learning_text,
};
pub use complete::{
    CompleteResult, TaskCompletionResult, complete, format_text as format_complete_text,
};
pub use decisions::{
    DecisionDeclineResult, DecisionResolveResult, DecisionRevertResult, DecisionSummary,
    DecisionsListResult, decline_decision_cmd, format_decline_text,
    format_list_text as format_decisions_list_text, format_resolve_text, format_revert_text,
    list_decisions, resolve_decision_cmd, revert_decision_cmd,
};
pub use doctor::{
    DoctorResult, DoctorSummary, Fix, Issue, IssueType, SetupAuditResult, audit_setup, doctor,
    format_doctor_verbose, format_setup_text, format_text as format_doctor_text,
};
pub use export::{ExportResult, export, format_text as format_export_text};
pub use fail::{FailResult, TaskFailResult, fail, format_text as format_fail_text};
pub use history::{
    HistoryResult, RunDetailResult, RunSummary, TaskAttempt,
    format_detail_text as format_history_detail_text, format_text as format_history_text, history,
    history_detail,
};
pub use import_learnings::{
    ImportLearningsResult, format_text as format_import_learnings_text, import_learnings,
};
pub use init::{
    DryRunDeletePreview, InitResult, PrefixMode, format_init_verbose,
    format_text as format_init_text, init,
};
pub use invalidate_learning::{
    InvalidateLearningResult, format_text as format_invalidate_learning_text, invalidate_learning,
};
pub use irrelevant::{
    IrrelevantResult, TaskIrrelevantResult, format_text as format_irrelevant_text, irrelevant,
};
pub use learn::{LearnParams, LearnResult, format_text as format_learn_text, learn};
pub use learnings::{
    LearningSummary as LearningsLearningSummary, LearningsListParams, LearningsListResult,
    format_text as format_learnings_text, list_learnings,
};
pub use list::{ListResult, TaskSummary, format_text as format_list_text, list};
pub use migrate::{
    MigrateResult, MigrationInfo, StatusResult, all as migrate_all, down as migrate_down_cmd,
    format_migrate_text, format_status_text, status as migrate_status, up as migrate_up_cmd,
};
pub use next::{
    CONFLICT_PENALTY, CandidateSummary, ClaimMetadata, DecayWarning, FILE_OVERLAP_SCORE,
    LearningSummaryOutput, NextResult, NextTaskOutput, PRIORITY_BASE, SYNERGY_BONUS,
    ScoreBreakdown, ScoreOutput, ScoredTask, SelectionMetadata, SelectionResult, apply_decay,
    find_decay_warnings, format_next_text, format_next_verbose,
    format_text as format_selection_text, next, select_next_task,
};
pub use recall::{
    LearningSummary as RecallLearningSummary, RecallCmdParams, RecallCmdResult,
    format_text as format_recall_text, format_verbose as format_recall_verbose, recall,
};
pub use reset::{
    ResetResult, TaskResetResult, count_resettable_tasks, format_text as format_reset_text,
    reset_all_tasks, reset_tasks,
};
pub use review::{
    ReviewAction, ReviewActionType, ReviewOptions, ReviewResult, ReviewTask, auto_unblock_all,
    format_text as format_review_text, get_reviewable_tasks, resolve_task,
};
pub use run::{
    BeginResult, EndResult, UpdateResult, begin, end, format_begin_text, format_end_text,
    format_update_text, update,
};
pub use show::{ShowResult, format_text as format_show_text, show};
pub use skip::{SkipResult, TaskSkipResult, format_text as format_skip_text, skip};
pub use stats::{
    ActiveRunInfo, LearningCounts, StatsResult, TaskCounts, format_text as format_stats_text, stats,
};
pub use unblock::{
    UnblockResult, UnskipResult, format_unblock_text, format_unskip_text, unblock, unskip,
};
pub use worktrees::{
    WorktreesResult, format_text as format_worktrees_text, list as worktrees_list,
    prune as worktrees_prune, remove as worktrees_remove,
};

/// Truncate a string to at most `max_chars` Unicode characters, appending "..." if truncated.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}
