//! `task-mgr enhance` — manage the task-mgr-fenced workflow block inside
//! `CLAUDE.md` and `AGENTS.md` (default targets) or any file passed via
//! `--target`.
//!
//! Subcommands:
//! - `agents` — write or update the marker-fenced block. Idempotent.
//! - `show`   — render the chosen profile to stdout. Never writes.
//! - `strip`  — remove the marker block (and its markers) from target files.
//!
//! ## Invariants
//!
//! 1. **Single splice entrypoint**: every marker manipulation goes through
//!    [`task_mgr::util::marker_splice::splice_block`]. No code in this module
//!    constructs or splits the marker pair itself.
//! 2. **Single write entrypoint**: every byte that lands on disk is written
//!    through [`task_mgr::util::marker_splice::write_atomic`]. Bypassing it
//!    risks torn writes on crash.
//! 3. **Symlinks are refused**, mirroring [`crate::commands::doctor`]'s
//!    safety guard for `CLAUDE.md`. Writing through a symlink would let an
//!    attacker pivot to any path the running user can write.
//! 4. **Out-of-scope files are never touched**: only paths listed in
//!    `--target` (or the default-targets resolution result) are read or
//!    written.
//!
//! ## Result shape
//!
//! Each target invocation produces a [`TargetOutcome`] (name + action +
//! error). [`EnhanceResult`] is a vector of these plus the command kind, so
//! `--format json` consumers can drive their own tooling on top.

pub mod templates;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::TaskMgrError;
use crate::util::marker_splice::{splice_block, write_atomic};
use templates::{EnhanceProfile, MARKER_BEGIN, MARKER_END};

/// Default targets when `--target` is not passed.
///
/// Order is preserved in the result. Touched only when the file already
/// exists, OR `--create` is set.
const DEFAULT_TARGETS: &[&str] = &["CLAUDE.md", "AGENTS.md"];

/// Which action was taken on a single target file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionTaken {
    /// File did not exist; `--create` was set; new file written with marker
    /// block at the top.
    Created,
    /// File existed without markers; block appended at the end.
    Appended,
    /// File existed with markers; marker contents replaced.
    Replaced,
    /// File existed with markers; content was identical post-splice, no
    /// write performed.
    NoOp,
    /// File missing and `--create` was not set; nothing was done.
    Skipped,
    /// `--dry-run` was set; the planned content is in `preview`.
    DryRun,
    /// `enhance strip` on a file that contained the marker block; markers
    /// and block content removed.
    Stripped,
    /// `enhance strip` on a file with no marker block; no change.
    NothingToStrip,
    /// Operation failed; see `error` for details.
    Errored,
}

impl ActionTaken {
    /// Whether this outcome should be treated as a success for the purposes
    /// of the process exit code.
    fn is_success(&self) -> bool {
        !matches!(self, ActionTaken::Skipped | ActionTaken::Errored)
    }
}

/// Per-target outcome from an enhance subcommand.
///
/// Modeled on [`crate::commands::doctor::setup_output::SetupFix`] so the
/// existing JSON / text formatters can stay shape-consistent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    /// Filesystem path of the target.
    pub target: PathBuf,
    /// Action taken on this target.
    pub action: ActionTaken,
    /// Optional planned content (only populated by `--dry-run`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Optional human-readable error message (populated when
    /// `action == Errored`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TargetOutcome {
    fn ok(target: PathBuf, action: ActionTaken) -> Self {
        Self {
            target,
            action,
            preview: None,
            error: None,
        }
    }

    fn errored(target: PathBuf, error: impl Into<String>) -> Self {
        Self {
            target,
            action: ActionTaken::Errored,
            preview: None,
            error: Some(error.into()),
        }
    }
}

/// Which enhance subcommand produced this result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnhanceKind {
    Agents,
    Show,
    Strip,
}

/// Aggregate result of an enhance invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhanceResult {
    /// Which subcommand ran.
    pub kind: EnhanceKind,
    /// Profile rendered (only meaningful for `agents` / `show`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<EnhanceProfile>,
    /// Whether this was a dry run (no file writes).
    pub dry_run: bool,
    /// Per-target outcomes (empty for `show`).
    #[serde(default)]
    pub targets: Vec<TargetOutcome>,
    /// Rendered profile body for `show`. None for `agents` / `strip`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
}

impl EnhanceResult {
    /// Returns `true` if any non-skipped target succeeded — i.e., the
    /// process should exit 0.
    pub fn any_success(&self) -> bool {
        // `show` produces no targets but is always a success when reached.
        if self.kind == EnhanceKind::Show {
            return true;
        }
        self.targets.iter().any(|t| t.action.is_success())
    }

    /// Returns `true` if every target either succeeded or was skipped (no
    /// errors). Used to distinguish exit 0 (clean) from exit 2 (some
    /// failures).
    pub fn no_errors(&self) -> bool {
        self.targets
            .iter()
            .all(|t| t.action != ActionTaken::Errored)
    }
}

/// Parameters for `enhance agents`.
#[derive(Debug, Clone)]
pub struct AgentsParams {
    /// Explicit target file list. Empty → default to `CLAUDE.md` + `AGENTS.md`.
    pub targets: Vec<PathBuf>,
    /// When `true`, render the result without writing to disk.
    pub dry_run: bool,
    /// When `true`, create missing files instead of skipping them.
    pub create: bool,
    /// Which profile to render.
    pub profile: EnhanceProfile,
    /// Working directory for default-target resolution. Tests pass an
    /// isolated tempdir; CLI passes `std::env::current_dir()`.
    pub cwd: PathBuf,
}

/// Parameters for `enhance strip`.
#[derive(Debug, Clone)]
pub struct StripParams {
    /// Explicit target file list. Empty → default to `CLAUDE.md` + `AGENTS.md`.
    pub targets: Vec<PathBuf>,
    /// When `true`, render the result without writing to disk.
    pub dry_run: bool,
    /// Working directory for default-target resolution.
    pub cwd: PathBuf,
}

/// Parameters for `enhance show`.
#[derive(Debug, Clone)]
pub struct ShowParams {
    pub profile: EnhanceProfile,
}

/// Resolve the list of target paths: caller-supplied paths if non-empty,
/// otherwise [`DEFAULT_TARGETS`] anchored at `cwd`.
fn resolve_targets(explicit: &[PathBuf], cwd: &Path) -> Vec<PathBuf> {
    if !explicit.is_empty() {
        return explicit.iter().map(|p| p.to_path_buf()).collect();
    }
    DEFAULT_TARGETS.iter().map(|name| cwd.join(name)).collect()
}

/// Check `path` for a symlink without following it. Returns `false` if the
/// path doesn't exist or metadata can't be read.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Detect unbalanced markers — exactly one of BEGIN / END present, or
/// END before BEGIN. Returns a descriptive error string, or `None` when
/// the markers are balanced (zero pairs OR one well-ordered pair).
fn detect_unbalanced(content: &str) -> Option<String> {
    let begin_count = content.matches(MARKER_BEGIN).count();
    let end_count = content.matches(MARKER_END).count();
    if begin_count != end_count {
        return Some(format!(
            "unbalanced markers: found {begin_count} BEGIN and {end_count} END"
        ));
    }
    if begin_count == 1
        && let (Some(bi), Some(ei)) = (content.find(MARKER_BEGIN), content.find(MARKER_END))
        && ei <= bi
    {
        return Some("unbalanced markers: END appears before BEGIN".to_string());
    }
    None
}

/// Loaded state for `enhance agents`: the current file contents (empty when
/// the file did not exist) plus the existence flag the writer needs.
struct AgentsLoaded {
    current: String,
    existed: bool,
}

/// Read-and-validate phase: symlink check, existence + `--create` check,
/// file read, balanced-markers check. Returns `Ok(loaded)` on proceed or
/// `Err(outcome)` for any early-return state (skipped / errored).
fn agents_load(target: &Path, create: bool) -> Result<AgentsLoaded, TargetOutcome> {
    let target_pb = target.to_path_buf();

    if is_symlink(target) {
        return Err(TargetOutcome::errored(
            target_pb,
            format!("{} is a symlink — refusing to write", target.display()),
        ));
    }

    let existed = target.exists();
    if !existed && !create {
        return Err(TargetOutcome::ok(target_pb, ActionTaken::Skipped));
    }

    let current = if existed {
        fs::read_to_string(target).map_err(|e| {
            TargetOutcome::errored(
                target_pb.clone(),
                format!("reading {} failed: {e}", target.display()),
            )
        })?
    } else {
        String::new()
    };

    if let Some(msg) = detect_unbalanced(&current) {
        return Err(TargetOutcome::errored(
            target_pb,
            format!("{}: {msg}", target.display()),
        ));
    }

    Ok(AgentsLoaded { current, existed })
}

/// Compute-and-write phase: splice the new body in, classify the action,
/// honor `dry_run` / no-op, and write atomically when work is needed.
fn agents_apply(target: &Path, loaded: &AgentsLoaded, body: &str, dry_run: bool) -> TargetOutcome {
    let target_pb = target.to_path_buf();
    let had_block = loaded.current.contains(MARKER_BEGIN) && loaded.current.contains(MARKER_END);
    let new_content = splice_block(&loaded.current, MARKER_BEGIN, MARKER_END, body);

    let planned_action = if !loaded.existed {
        ActionTaken::Created
    } else if had_block {
        if new_content == loaded.current {
            ActionTaken::NoOp
        } else {
            ActionTaken::Replaced
        }
    } else {
        ActionTaken::Appended
    };

    if dry_run {
        return TargetOutcome {
            target: target_pb,
            action: ActionTaken::DryRun,
            preview: Some(new_content),
            error: None,
        };
    }

    if planned_action == ActionTaken::NoOp {
        // Idempotent path: skip the disk write entirely so mtime is preserved.
        return TargetOutcome::ok(target_pb, ActionTaken::NoOp);
    }

    if !loaded.existed
        && let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        return TargetOutcome::errored(
            target_pb,
            format!("parent directory {} does not exist", parent.display()),
        );
    }

    match write_atomic(target, &new_content) {
        Ok(()) => TargetOutcome::ok(target_pb, planned_action),
        Err(e) => TargetOutcome::errored(
            target_pb,
            format!("writing {} failed: {e}", target.display()),
        ),
    }
}

/// Apply the splice for `enhance agents`, handling the create / append /
/// replace / dry-run / no-op variants.
fn agents_one(target: &Path, body: &str, create: bool, dry_run: bool) -> TargetOutcome {
    match agents_load(target, create) {
        Ok(loaded) => agents_apply(target, &loaded, body, dry_run),
        Err(outcome) => outcome,
    }
}

/// Remove the marker block (and its markers) from a single target.
fn strip_one(target: &Path, dry_run: bool) -> TargetOutcome {
    let target_pb = target.to_path_buf();

    if is_symlink(target) {
        return TargetOutcome::errored(
            target_pb,
            format!("{} is a symlink — refusing to write", target.display()),
        );
    }

    if !target.exists() {
        return TargetOutcome::ok(target_pb, ActionTaken::Skipped);
    }

    let current = match fs::read_to_string(target) {
        Ok(s) => s,
        Err(e) => {
            return TargetOutcome::errored(
                target_pb,
                format!("reading {} failed: {e}", target.display()),
            );
        }
    };

    if let Some(msg) = detect_unbalanced(&current) {
        return TargetOutcome::errored(target_pb, format!("{}: {msg}", target.display()));
    }

    let new_content = match remove_marker_block(&current) {
        Some(updated) => updated,
        None => return TargetOutcome::ok(target_pb, ActionTaken::NothingToStrip),
    };

    if dry_run {
        return TargetOutcome {
            target: target_pb,
            action: ActionTaken::DryRun,
            preview: Some(new_content),
            error: None,
        };
    }

    if new_content == current {
        return TargetOutcome::ok(target_pb, ActionTaken::NoOp);
    }

    match write_atomic(target, &new_content) {
        Ok(()) => TargetOutcome::ok(target_pb, ActionTaken::Stripped),
        Err(e) => TargetOutcome::errored(
            target_pb,
            format!("writing {} failed: {e}", target.display()),
        ),
    }
}

/// Remove `MARKER_BEGIN .. MARKER_END` (and any single trailing newline)
/// from `content`. Returns `Some(updated)` when a block was removed; `None`
/// when neither marker is present.
///
/// Only the first occurrence of the pair is removed (mirrors the
/// "first valid pair wins" rule in `splice_block`).
fn remove_marker_block(content: &str) -> Option<String> {
    let bi = content.find(MARKER_BEGIN)?;
    let ei = content.find(MARKER_END)?;
    if ei <= bi {
        return None;
    }
    let after_end = ei + MARKER_END.len();
    let mut tail_start = after_end;
    // Eat at most one trailing newline so the surrounding text doesn't get
    // a stray blank line where the block used to sit.
    if content.as_bytes().get(tail_start) == Some(&b'\n') {
        tail_start += 1;
    }
    let head = &content[..bi];
    let tail = &content[tail_start..];
    // Trim a trailing newline left on `head` only when we ate the marker's
    // own trailing newline AND the head ended with a newline; otherwise
    // honor whatever spacing the user had.
    let stripped = format!("{head}{tail}");
    Some(stripped)
}

/// Entry point for `task-mgr enhance agents`.
pub fn enhance_agents(params: AgentsParams) -> Result<EnhanceResult, TaskMgrError> {
    let body = params.profile.body();
    let targets = resolve_targets(&params.targets, &params.cwd);

    let mut outcomes = Vec::with_capacity(targets.len());
    for target in &targets {
        let outcome = agents_one(target, body, params.create, params.dry_run);
        if matches!(outcome.action, ActionTaken::Skipped) {
            eprintln!(
                "skipping {} (not found; pass --create to create)",
                outcome.target.display()
            );
        } else if let Some(err) = &outcome.error {
            eprintln!("enhance agents: {err}");
        }
        outcomes.push(outcome);
    }

    Ok(EnhanceResult {
        kind: EnhanceKind::Agents,
        profile: Some(params.profile),
        dry_run: params.dry_run,
        targets: outcomes,
        rendered: None,
    })
}

/// Entry point for `task-mgr enhance show`. Renders the profile body to
/// stdout and returns it in the result. Never writes to disk.
pub fn enhance_show(params: ShowParams) -> Result<EnhanceResult, TaskMgrError> {
    let body = params.profile.body();
    Ok(EnhanceResult {
        kind: EnhanceKind::Show,
        profile: Some(params.profile),
        dry_run: false,
        targets: Vec::new(),
        rendered: Some(body.to_string()),
    })
}

/// Entry point for `task-mgr enhance strip`.
pub fn enhance_strip(params: StripParams) -> Result<EnhanceResult, TaskMgrError> {
    let targets = resolve_targets(&params.targets, &params.cwd);

    let mut outcomes = Vec::with_capacity(targets.len());
    for target in &targets {
        let outcome = strip_one(target, params.dry_run);
        if outcome.action == ActionTaken::NothingToStrip {
            eprintln!("no marker block found in {}", outcome.target.display());
        } else if let Some(err) = &outcome.error {
            eprintln!("enhance strip: {err}");
        }
        outcomes.push(outcome);
    }

    Ok(EnhanceResult {
        kind: EnhanceKind::Strip,
        profile: None,
        dry_run: params.dry_run,
        targets: outcomes,
        rendered: None,
    })
}

/// Human-readable formatter used by `TextFormattable` in handlers.
pub fn format_text(result: &EnhanceResult) -> String {
    let mut out = String::new();
    match result.kind {
        EnhanceKind::Show => {
            if let Some(body) = &result.rendered {
                out.push_str(body);
            }
            return out;
        }
        EnhanceKind::Agents => {
            out.push_str("=== Enhance agents ===\n");
        }
        EnhanceKind::Strip => {
            out.push_str("=== Enhance strip ===\n");
        }
    }
    if result.dry_run {
        out.push_str("(dry-run; no files were written)\n");
    }
    for outcome in &result.targets {
        let label = match outcome.action {
            ActionTaken::Created => "created",
            ActionTaken::Appended => "appended",
            ActionTaken::Replaced => "replaced",
            ActionTaken::NoOp => "no-op",
            ActionTaken::Skipped => "skipped",
            ActionTaken::DryRun => "dry-run",
            ActionTaken::Stripped => "stripped",
            ActionTaken::NothingToStrip => "nothing-to-strip",
            ActionTaken::Errored => "error",
        };
        out.push_str(&format!("  [{label}] {}", outcome.target.display()));
        if let Some(err) = &outcome.error {
            out.push_str(&format!(" — {err}"));
        }
        out.push('\n');
    }
    out
}
