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

pub mod apply_learning;
pub mod complete;
pub mod doctor;
pub mod export;
pub mod fail;
pub mod history;
pub mod import_learnings;
pub mod init;
pub mod irrelevant;
pub mod learn;
pub mod learnings;
pub mod list;
pub mod migrate;
pub mod next;
pub mod recall;
pub mod reset;
pub mod review;
pub mod run;
pub mod show;
pub mod skip;
pub mod stats;
pub mod unblock;

pub use apply_learning::{
    apply_learning, format_text as format_apply_learning_text, ApplyLearningResult,
};
pub use complete::{
    complete, format_text as format_complete_text, CompleteResult, TaskCompletionResult,
};
pub use doctor::{
    doctor, format_doctor_verbose, format_text as format_doctor_text, DoctorResult, DoctorSummary,
    Fix, Issue, IssueType,
};
pub use export::{export, format_text as format_export_text, ExportResult};
pub use fail::{fail, format_text as format_fail_text, FailResult, TaskFailResult};
pub use history::{
    format_detail_text as format_history_detail_text, format_text as format_history_text, history,
    history_detail, HistoryResult, RunDetailResult, RunSummary, TaskAttempt,
};
pub use import_learnings::{
    format_text as format_import_learnings_text, import_learnings, ImportLearningsResult,
};
pub use init::{
    format_init_verbose, format_text as format_init_text, init, DryRunDeletePreview, InitResult,
    PrefixMode,
};
pub use irrelevant::{
    format_text as format_irrelevant_text, irrelevant, IrrelevantResult, TaskIrrelevantResult,
};
pub use learn::{format_text as format_learn_text, learn, LearnParams, LearnResult};
pub use learnings::{
    format_text as format_learnings_text, list_learnings,
    LearningSummary as LearningsLearningSummary, LearningsListParams, LearningsListResult,
};
pub use list::{format_text as format_list_text, list, ListResult, TaskSummary};
pub use migrate::{
    all as migrate_all, down as migrate_down_cmd, format_migrate_text, format_status_text,
    status as migrate_status, up as migrate_up_cmd, MigrateResult, MigrationInfo, StatusResult,
};
pub use next::{
    apply_decay, find_decay_warnings, format_next_text, format_next_verbose,
    format_text as format_selection_text, next, select_next_task, CandidateSummary, ClaimMetadata,
    DecayWarning, LearningSummaryOutput, NextResult, NextTaskOutput, ScoreBreakdown, ScoreOutput,
    ScoredTask, SelectionMetadata, SelectionResult, CONFLICT_PENALTY, FILE_OVERLAP_SCORE,
    PRIORITY_BASE, SYNERGY_BONUS,
};
pub use recall::{
    format_text as format_recall_text, format_verbose as format_recall_verbose, recall,
    LearningSummary as RecallLearningSummary, RecallCmdParams, RecallCmdResult,
};
pub use reset::{
    count_resettable_tasks, format_text as format_reset_text, reset_all_tasks, reset_tasks,
    ResetResult, TaskResetResult,
};
pub use review::{
    auto_unblock_all, format_text as format_review_text, get_reviewable_tasks, resolve_task,
    ReviewAction, ReviewActionType, ReviewOptions, ReviewResult, ReviewTask,
};
pub use run::{
    begin, end, format_begin_text, format_end_text, format_update_text, update, BeginResult,
    EndResult, UpdateResult,
};
pub use show::{format_text as format_show_text, show, ShowResult};
pub use skip::{format_text as format_skip_text, skip, SkipResult, TaskSkipResult};
pub use stats::{
    format_text as format_stats_text, stats, ActiveRunInfo, LearningCounts, StatsResult, TaskCounts,
};
pub use unblock::{
    format_unblock_text, format_unskip_text, unblock, unskip, UnblockResult, UnskipResult,
};
