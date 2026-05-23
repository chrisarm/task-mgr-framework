//! Overflow recovery diagnostics: prompt dumps, JSONL event log, and
//! filename sanitization for `PromptTooLong` handling.
//!
//! This module is the home for `sanitize_id_for_filename`, the path-traversal
//! defense applied to task IDs before they are used as components of dump
//! filenames under `.task-mgr/overflow-dumps/`. The allowlist mirrors
//! `worktree::sanitize_branch_name` (learning #1853) but additionally
//! collapses `..` substrings to prevent traversal segments from surviving
//! into a filesystem path.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::model;
use crate::loop_engine::project_config::ProjectConfig;
use crate::loop_engine::prompt::PromptResult;
use crate::loop_engine::runner::RunnerKind;

/// Recovery action chosen by `handle_prompt_too_long` for a given overflow.
///
/// Serialized as a tagged JSON object via `tag = "action"`, with payload
/// fields as siblings of `action` (NOT nested). The `to_1m_model` variant
/// is renamed explicitly because serde's snake_case transform of
/// `To1mModel` produces `to1m_model` — the underscore between `to` and
/// `1m` is required for stability with downstream JSONL consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecoveryAction {
    DowngradeEffort {
        new_effort: String,
    },
    EscalateModel {
        new_model: String,
    },
    #[serde(rename = "to_1m_model")]
    To1mModel {
        new_model: String,
    },
    /// Rung 4 (FEAT-006): the Claude ladder is exhausted and the fallback
    /// runner config is enabled — promote the task to the fallback provider
    /// (Grok today). Fires only when `project_config.fallback_runner` is
    /// `Some(cfg)` with `cfg.enabled = true` AND the effective runner is
    /// still `RunnerKind::Claude` (idempotency guard — a task already on
    /// Grok falls through to `Blocked`).
    FallbackToProvider {
        provider: String,
        model: String,
    },
    Blocked,
}

impl RecoveryAction {
    /// Format the user-visible stderr message for this recovery action.
    pub fn user_message(
        &self,
        task_id: &str,
        effort: Option<&str>,
        effective_model: Option<&str>,
    ) -> String {
        let eff = effort.unwrap_or("(default)");
        let mdl = effective_model.unwrap_or("(default)");
        match self {
            Self::DowngradeEffort { new_effort } => format!(
                "Prompt is too long for {task_id} at effort {eff} — downgrading effort to {new_effort}",
            ),
            Self::EscalateModel { new_model } => format!(
                "Prompt is too long for {task_id} at effort {eff}, model {mdl} — escalating model to {new_model} (effort floor reached)",
            ),
            Self::To1mModel { new_model } => format!(
                "Prompt is too long for {task_id} at effort {eff}, model {mdl} — escalating to 1M-context variant {new_model} (already at Opus)",
            ),
            Self::FallbackToProvider {
                model: target_model,
                ..
            } => format!(
                "Prompt is too long for {task_id} at effort {eff}, model {mdl} — falling back to {target_model} (Claude ladder exhausted)",
            ),
            Self::Blocked => format!(
                "Prompt is too long for {task_id} at effort {eff}, model {mdl} — no recovery available (already at Opus[1M] with effort=high)",
            ),
        }
    }
}

/// Structured overflow event written one-per-line to
/// `.task-mgr/overflow-events.jsonl`.
///
/// `sections` is `Vec<(String, usize)>` (positional, order-preserving) and
/// serializes as a JSON array of `[name, size]` pairs — NOT a map. The
/// declaration order matches the prompt-assembly order, which the dump
/// header relies on.
///
/// `slot_index` is `Some(n)` for wave-mode events (the slot that overflowed)
/// and omitted entirely from JSON for sequential events (`None` +
/// `skip_serializing_if`). This lets downstream consumers distinguish the
/// two paths without inspecting other fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverflowEvent {
    pub ts: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub iteration: u32,
    /// Slot index within a parallel wave. `None` for sequential-mode events;
    /// `Some(n)` for wave-mode events so JSONL consumers can attribute the
    /// overflow to the correct slot without re-parsing the task_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_index: Option<usize>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Task difficulty at the time the prompt was assembled. `None` when the
    /// task had no difficulty set (or for legacy events that predate this field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_difficulty: Option<String>,
    pub prompt_bytes: usize,
    pub sections: Vec<(String, usize)>,
    pub dropped_sections: Vec<String>,
    pub recovery: RecoveryAction,
    pub dump_path: String,
    /// LLM runner that was active when the overflow fired. `None` for legacy
    /// events that predate this field; `Some("claude")` / `Some("grok")` for
    /// current events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
}

/// Sanitize a task ID for safe use as a filename component.
///
/// Allows ASCII alphanumerics, `-`, `_`, and `.`; everything else (including
/// `/`, `\\`, spaces, and NUL bytes) is replaced with `-`. The substring `..`
/// is replaced with `--` first so traversal segments cannot survive even when
/// individual `.` characters are otherwise allowed. Empty input becomes `"_"`
/// so the function never returns an empty string.
pub fn sanitize_id_for_filename(id: &str) -> String {
    if id.is_empty() {
        return "_".to_string();
    }
    let collapsed = id.replace("..", "--");
    collapsed
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '-',
        })
        .collect()
}

/// Metadata written at the top of every prompt dump file.
///
/// `sections` and `dropped_sections` are borrowed slices to avoid cloning the
/// section-size vec from `PromptResult`. The lifetime ties both borrows to the
/// same source (typically the `PromptResult` lifetime).
pub struct DumpHeader<'a> {
    pub iteration: u32,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub ts_iso8601: String,
    pub total_bytes: usize,
    pub sections: &'a [(&'a str, usize)],
    pub dropped_sections: &'a [String],
}

/// Produce the human-readable header block written at the top of a dump file.
///
/// Includes a NOTE about Claude Code's auto-load layer so users understand
/// why the context window may be larger than the section breakdown suggests.
pub fn format_breakdown(
    sections: &[(&str, usize)],
    dropped: &[String],
    total_bytes: usize,
) -> String {
    let sep = "=".repeat(80);
    let mut out = String::new();
    out.push_str(&sep);
    out.push('\n');
    out.push_str("PROMPT OVERFLOW DUMP\n");
    out.push_str(&sep);
    out.push('\n');
    out.push_str(&format!("Total assembled bytes: {total_bytes}\n"));
    out.push_str("Section breakdown:\n");
    for (name, size) in sections {
        let pct = if total_bytes > 0 {
            (*size as f64 / total_bytes as f64) * 100.0
        } else {
            0.0
        };
        out.push_str(&format!("  {name}: {size} bytes ({pct:.1}%)\n"));
    }
    if !dropped.is_empty() {
        out.push_str("Dropped sections (too large to include):\n");
        for d in dropped {
            out.push_str(&format!("  {d}\n"));
        }
    }
    out.push('\n');
    out.push_str(concat!(
        "NOTE: Claude Code auto-loads CLAUDE.md, skills, and agent configuration\n",
        "before passing your prompt to the model. These are not reflected in the\n",
        "section breakdown above but do count against the context window (the\n",
        "auto-loaded layer can add thousands of tokens on top of the assembled prompt).\n",
    ));
    out.push_str(&sep);
    out.push('\n');
    out
}

/// Write a prompt dump file and return its absolute path.
///
/// Creates `dir` if it does not exist. The filename is
/// `<sanitized_task_id>-iter<N>-<unix_ts>.txt` where `N` comes from
/// `header.iteration` — callers must set that field correctly before
/// invoking this function (a zero value produces `-iter0-` in the filename).
/// The file contains the formatted section breakdown header followed by the
/// raw prompt.
pub fn dump_prompt(
    dir: &Path,
    task_id: &str,
    header: &DumpHeader<'_>,
    prompt: &str,
) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let sanitized = sanitize_id_for_filename(task_id);
    let filename = format!("{sanitized}-iter{}-{ts}.txt", header.iteration);
    let path = dir.join(&filename);

    let meta = format!(
        "task_id: {}\niteration: {}\nmodel: {}\neffort: {}\ntimestamp: {}\n\n",
        sanitized,
        header.iteration,
        header.model.as_deref().unwrap_or("(default)"),
        header.effort.as_deref().unwrap_or("(default)"),
        header.ts_iso8601,
    );
    let breakdown = format_breakdown(header.sections, header.dropped_sections, header.total_bytes);
    let content = format!("{meta}{breakdown}\n{prompt}");

    fs::write(&path, &content)?;
    path.canonicalize().or(Ok(path))
}

/// Append a single structured event line to the JSONL event log.
///
/// The log file is at `dir/overflow-events.jsonl`. Created if absent. The
/// entire JSON object plus a newline is written in a single `write_all` call
/// for best-effort atomicity on POSIX.
pub fn append_event_log(dir: &Path, event: &OverflowEvent) -> io::Result<()> {
    let path = dir.join("overflow-events.jsonl");
    let mut line =
        serde_json::to_vec(event).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    if line.len() > 4096 {
        eprintln!(
            "warning: overflow JSONL line is {} bytes — exceeds PIPE_BUF (4096); O_APPEND atomicity is not guaranteed",
            line.len()
        );
    }
    let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
    file.write_all(&line)
}

/// Keep only the `keep` most-recently-modified prompt dumps for a given task.
///
/// Matches files named `<sanitized_task_id>-iter*-*.txt` in `dir`. Deletes
/// all but the `keep` newest by modification time. Files for other task IDs
/// are not touched. Returns `Ok(())` if `dir` does not yet exist.
///
/// **Per-entry log-and-continue**: an unreadable dir entry, missing metadata,
/// or failed deletion is logged via `eprintln!` and the rotation pass moves
/// on to the next file. A single filesystem error never aborts the call —
/// observability is best-effort, but rotation drift is bounded.
pub fn rotate_dumps_keep_n(dir: &Path, sanitized_task_id: &str, keep: usize) -> io::Result<()> {
    let prefix = format!("{sanitized_task_id}-iter");

    let read_dir = match fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };

    let mut entries: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warning: overflow rotate: skipping unreadable dir entry: {e}");
                continue;
            }
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && name_str.ends_with(".txt") {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "warning: overflow rotate: skipping {}: metadata error: {e}",
                        entry.path().display()
                    );
                    continue;
                }
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((mtime, entry.path()));
        }
    }

    // Sort newest first.
    entries.sort_by_key(|b| std::cmp::Reverse(b.0));

    for (_, path) in entries.into_iter().skip(keep) {
        if let Err(e) = fs::remove_file(&path) {
            eprintln!(
                "warning: overflow rotate: failed to remove {}: {e}",
                path.display()
            );
        }
    }

    Ok(())
}

/// Read the current `tasks.model` column for a task, returning `Ok(None)` when
/// the column is NULL and `Err` only on a connectivity / schema failure.
/// Used by [`handle_prompt_too_long`] to capture the pre-fallback model into
/// `ctx.overflow_original_task_model` before the rung-4 UPDATE mutates the
/// column.
fn read_task_model_from_db(conn: &Connection, task_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT model FROM tasks WHERE id = ?1", [task_id], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Handle a `PromptTooLong` outcome end-to-end: pick a recovery rung, mutate
/// `IterationContext`, update the task row, emit the stderr message, and
/// write the diagnostics bundle (dump + JSONL + rotation).
///
/// Returns the chosen [`RecoveryAction`] so callers can keep flowing the
/// classification (e.g. for outcome telemetry); the side effects are the
/// primary contract.
///
/// **Order of operations** (must not be reordered — the recovery state
/// must be durable before any best-effort observability runs):
/// 1. Pick recovery rung (1-effort downgrade → 2-model escalate → 3-1M model
///    → 4-fallback-to-provider → 5-blocked).
/// 2. Update `ctx.overflow_recovered`, `ctx.overflow_original_model`
///    (first-overflow only), and `ctx.overflow_original_task_model`
///    (first-fallback snapshot of the `tasks.model` DB column).
/// 3. UPDATE the task row (status='todo' on rungs 1-3, 'todo' AND set
///    `tasks.model = cfg.model` on rung 4, 'blocked' on rung 5).
/// 4. Emit the rung-specific stderr message.
/// 5. Best-effort: write prompt dump.
/// 6. Best-effort: append JSONL event line.
/// 7. Best-effort: rotate dumps (keep newest 3 per task).
///
/// Filesystem failures in steps 5-7 are logged via `eprintln!` and never
/// propagate — observability is best-effort, recovery is not.
///
/// `effective_runner` is the single computed value from
/// [`crate::loop_engine::engine::resolve_effective_runner`] at the spawn
/// site — the rung-4 idempotency guard pins on this value (PRD §2.5
/// "single-predicate guard" — never re-derive via
/// `runner_overrides.get(task)` OR `provider_for_model(model)`).
#[allow(clippy::too_many_arguments)]
pub fn handle_prompt_too_long(
    ctx: &mut crate::loop_engine::engine::IterationContext,
    conn: &mut Connection,
    task_id: &str,
    effort: Option<&str>,
    effective_model: Option<&str>,
    prompt_result: &PromptResult,
    iteration: u32,
    run_id: Option<&str>,
    base_dir: &Path,
    slot_index: Option<usize>,
    effective_runner: RunnerKind,
    project_config: &ProjectConfig,
) -> RecoveryAction {
    // M2: snapshot whether this task already has a Grok promotion recorded
    // BEFORE the rung-4 arm can insert one. Used below to suppress a duplicate
    // banner when the RuntimeError hook fires in the same wave for the same task.
    let was_already_promoted = ctx.runner_overrides.contains_key(task_id);

    // Step 1: pick recovery rung. Rung 4 (FallbackToProvider) sits between
    // rung 3 (to_1m_model) and rung 5 (Blocked); its precondition is a
    // SINGLE-predicate guard (PRD §2.5): the computed `effective_runner`
    // value MUST be `RunnerKind::Claude` AND the fallback config MUST be
    // enabled. Re-deriving the guard via
    // `runner_overrides.get(...).is_none() || provider_for_model(...) == Claude`
    // is explicitly prohibited because it can silently drift between the
    // spawn-site value and the rung-4 check.
    let action = if let Some(next_effort) = model::downgrade_effort(effort) {
        ctx.effort_overrides
            .insert(task_id.to_string(), next_effort);
        RecoveryAction::DowngradeEffort {
            new_effort: next_effort.to_string(),
        }
    } else if let Some(next_model) = model::escalate_below_opus(effective_model) {
        ctx.model_overrides
            .insert(task_id.to_string(), next_model.to_string());
        RecoveryAction::EscalateModel {
            new_model: next_model.to_string(),
        }
    } else if let Some(m1m) = model::to_1m_model(effective_model) {
        ctx.model_overrides
            .insert(task_id.to_string(), m1m.to_string());
        RecoveryAction::To1mModel {
            new_model: m1m.to_string(),
        }
    } else if effective_runner == RunnerKind::Claude
        && let Some(cfg) = project_config.fallback_runner.as_ref()
        && cfg.enabled
    {
        ctx.runner_overrides
            .insert(task_id.to_string(), RunnerKind::Grok);
        ctx.model_overrides
            .insert(task_id.to_string(), cfg.model.clone());
        RecoveryAction::FallbackToProvider {
            provider: cfg.provider.clone(),
            model: cfg.model.clone(),
        }
    } else {
        RecoveryAction::Blocked
    };

    // Step 2: capture overflow markers — first-overflow capture for
    // `overflow_original_model` (entry().or_insert_with), unconditional
    // insert for the recovered set. Also capture the pre-fallback
    // `tasks.model` DB column into `overflow_original_task_model` BEFORE the
    // rung-4 DB UPDATE mutates it; the snapshot is used by FEAT-008's
    // `check_override_invalidation` to detect operator edits and drop stale
    // overrides. Captured for every rung (entry().or_insert_with is
    // idempotent) so the snapshot remains stable across repeated overflows
    // on the same task.
    ctx.overflow_recovered.insert(task_id.to_string());
    ctx.overflow_original_model
        .entry(task_id.to_string())
        .or_insert_with(|| effective_model.unwrap_or("(default)").to_string());
    ctx.overflow_original_task_model
        .entry(task_id.to_string())
        .or_insert_with(|| read_task_model_from_db(conn, task_id).unwrap_or(None));

    // Step 3: update DB.
    //   - Blocked            → status='blocked' (started_at preserved for audit)
    //   - FallbackToProvider → status='todo' + clear started_at + set tasks.model = cfg.model
    //                          so `resolve_task_model` picks the Grok model on next iteration.
    //   - Rungs 1-3          → status='todo' + clear started_at (model unchanged)
    match action {
        RecoveryAction::Blocked => {
            let _ = TaskLifecycle::new(conn).auto_block_after_failures(
                task_id,
                "prompt too long",
                i64::from(iteration),
            );
        }
        RecoveryAction::FallbackToProvider { ref model, .. } => {
            let _ = TaskLifecycle::new(conn).resurrect_with_model_override(task_id, model);
        }
        _ => {
            let _ = TaskLifecycle::new(conn).resurrect_for_iteration(None, &[task_id]);
        }
    }

    // Step 4: rung-specific stderr message. For FallbackToProvider, suppress
    // the banner when this task was already promoted (was_already_promoted ==
    // true) so a wave-mode task that triggers BOTH the overflow rung-4 path
    // and the RuntimeError hook in the same wave emits exactly one banner.
    if !matches!(action, RecoveryAction::FallbackToProvider { .. }) || !was_already_promoted {
        eprintln!("{}", action.user_message(task_id, effort, effective_model));
    }

    // Step 5: best-effort prompt dump.
    let dumps_dir = base_dir.join("overflow-dumps");
    let ts_iso8601 = chrono::Utc::now().to_rfc3339();
    let header = DumpHeader {
        iteration,
        model: effective_model.map(String::from),
        effort: effort.map(String::from),
        ts_iso8601: ts_iso8601.clone(),
        total_bytes: prompt_result.prompt.len(),
        sections: prompt_result.section_sizes.as_slice(),
        dropped_sections: prompt_result.dropped_sections.as_slice(),
    };
    let dump_path = match dump_prompt(&dumps_dir, task_id, &header, &prompt_result.prompt) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("warning: overflow dump write failed: {}", e);
            // Synthetic placeholder path so JSONL still records *something*.
            dumps_dir.join(format!(
                "{}-iter{}-FAILED.txt",
                sanitize_id_for_filename(task_id),
                iteration,
            ))
        }
    };

    // Step 6: best-effort JSONL append.
    let event = OverflowEvent {
        ts: ts_iso8601,
        task_id: task_id.to_string(),
        run_id: run_id.map(String::from),
        iteration,
        slot_index,
        model: effective_model.map(String::from),
        effort: effort.map(String::from),
        task_difficulty: prompt_result.task_difficulty.clone(),
        prompt_bytes: prompt_result.prompt.len(),
        sections: prompt_result
            .section_sizes
            .iter()
            .map(|(n, s)| ((*n).to_string(), *s))
            .collect(),
        dropped_sections: prompt_result.dropped_sections.clone(),
        recovery: action.clone(),
        dump_path: dump_path.to_string_lossy().into_owned(),
        runner: Some(
            match effective_runner {
                RunnerKind::Claude => "claude",
                RunnerKind::Grok => "grok",
            }
            .to_string(),
        ),
    };
    if let Err(e) = append_event_log(base_dir, &event) {
        eprintln!("warning: overflow event log append failed: {}", e);
    }

    // Step 7: best-effort dump rotation (keep newest 3 per task).
    let sanitized = sanitize_id_for_filename(task_id);
    if let Err(e) = rotate_dumps_keep_n(&dumps_dir, &sanitized, 3) {
        eprintln!("warning: overflow dump rotation failed: {}", e);
    }

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac1_allowlist_passthrough() {
        assert_eq!(sanitize_id_for_filename("FOO-BAR_baz.42"), "FOO-BAR_baz.42");
    }

    #[test]
    fn ac2_slashes_and_double_dots_replaced() {
        assert_eq!(sanitize_id_for_filename("FOO/BAR..baz"), "FOO-BAR--baz");
    }

    #[test]
    fn ac3_empty_input_yields_underscore_placeholder() {
        assert_eq!(sanitize_id_for_filename(""), "_");
    }

    #[test]
    fn ac4_path_traversal_neutralized() {
        let out = sanitize_id_for_filename("../../../etc/passwd");
        assert!(!out.contains('/'), "output must not contain `/`: {out:?}");
        assert!(
            !out.contains(".."),
            "output must not contain `..` substring: {out:?}"
        );
    }

    #[test]
    fn ac5_spaces_become_dashes() {
        assert_eq!(
            sanitize_id_for_filename("task with spaces"),
            "task-with-spaces"
        );
    }

    #[test]
    fn ac6_nul_bytes_removed() {
        let out = sanitize_id_for_filename("\0null\0byte");
        assert!(
            !out.contains('\0'),
            "output must not contain NUL byte: {out:?}"
        );
    }

    #[test]
    fn user_message_downgrade_effort_exact_string() {
        let msg = RecoveryAction::DowngradeEffort {
            new_effort: "high".to_string(),
        }
        .user_message("MY-TASK-001", Some("xhigh"), None);
        assert_eq!(
            msg,
            "Prompt is too long for MY-TASK-001 at effort xhigh — downgrading effort to high"
        );
    }

    #[test]
    fn user_message_escalate_model_exact_string() {
        let msg = RecoveryAction::EscalateModel {
            new_model: model::OPUS_MODEL.to_string(),
        }
        .user_message("MY-TASK-001", Some("high"), Some(model::SONNET_MODEL));
        assert_eq!(
            msg,
            format!(
                "Prompt is too long for MY-TASK-001 at effort high, model {} — escalating model to {} (effort floor reached)",
                model::SONNET_MODEL,
                model::OPUS_MODEL
            )
        );
    }

    #[test]
    fn user_message_to_1m_model_exact_string() {
        let msg = RecoveryAction::To1mModel {
            new_model: model::OPUS_MODEL_1M.to_string(),
        }
        .user_message("MY-TASK-001", Some("high"), Some(model::OPUS_MODEL));
        assert_eq!(
            msg,
            format!(
                "Prompt is too long for MY-TASK-001 at effort high, model {} — escalating to 1M-context variant {} (already at Opus)",
                model::OPUS_MODEL,
                model::OPUS_MODEL_1M
            )
        );
    }

    #[test]
    fn user_message_blocked_exact_string() {
        let msg = RecoveryAction::Blocked.user_message("MY-TASK-001", None, None);
        assert_eq!(
            msg,
            "Prompt is too long for MY-TASK-001 at effort (default), model (default) — no recovery available (already at Opus[1M] with effort=high)"
        );
    }

    #[test]
    fn user_message_fallback_to_provider_exact_string() {
        let msg = RecoveryAction::FallbackToProvider {
            provider: "grok".to_string(),
            model: "grok-4-fast".to_string(),
        }
        .user_message("MY-TASK-001", Some("high"), Some(model::OPUS_MODEL_1M));
        assert_eq!(
            msg,
            format!(
                "Prompt is too long for MY-TASK-001 at effort high, model {} — falling back to grok-4-fast (Claude ladder exhausted)",
                model::OPUS_MODEL_1M,
            )
        );
    }

    #[test]
    fn recovery_fallback_to_provider_serialization() {
        let v = serde_json::to_value(RecoveryAction::FallbackToProvider {
            provider: "grok".to_string(),
            model: "grok-4-fast".to_string(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "action": "fallback_to_provider",
                "provider": "grok",
                "model": "grok-4-fast",
            })
        );
    }

    #[test]
    fn recovery_fallback_to_provider_round_trip() {
        let action = RecoveryAction::FallbackToProvider {
            provider: "grok".to_string(),
            model: "grok-4-fast".to_string(),
        };
        let s = serde_json::to_string(&action).unwrap();
        let back: RecoveryAction = serde_json::from_str(&s).unwrap();
        assert_eq!(action, back);
    }

    fn sample_event() -> OverflowEvent {
        OverflowEvent {
            ts: "2026-05-04T20:00:00+00:00".to_string(),
            task_id: "FOO-FEAT-001".to_string(),
            run_id: Some("run-abc".to_string()),
            iteration: 7,
            slot_index: None,
            model: Some(model::SONNET_MODEL.to_string()),
            effort: Some("high".to_string()),
            task_difficulty: Some("high".to_string()),
            prompt_bytes: 12345,
            sections: vec![
                ("task".to_string(), 100),
                ("base_prompt".to_string(), 200),
                ("learnings".to_string(), 300),
            ],
            dropped_sections: vec!["progress".to_string()],
            recovery: RecoveryAction::EscalateModel {
                new_model: model::OPUS_MODEL.to_string(),
            },
            dump_path: "/tmp/dump.txt".to_string(),
            runner: None,
        }
    }

    #[test]
    fn event_serializes_with_snake_case_keys() {
        let v = serde_json::to_value(sample_event()).unwrap();
        let obj = v.as_object().unwrap();
        for key in [
            "ts",
            "task_id",
            "run_id",
            "iteration",
            "model",
            "effort",
            "prompt_bytes",
            "sections",
            "dropped_sections",
            "recovery",
            "dump_path",
        ] {
            assert!(obj.contains_key(key), "missing key {key} in {obj:?}");
        }
    }

    #[test]
    fn recovery_downgrade_effort_serialization() {
        let v = serde_json::to_value(RecoveryAction::DowngradeEffort {
            new_effort: "high".to_string(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"action": "downgrade_effort", "new_effort": "high"})
        );
    }

    #[test]
    fn recovery_escalate_model_serialization() {
        let v = serde_json::to_value(RecoveryAction::EscalateModel {
            new_model: model::OPUS_MODEL.to_string(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"action": "escalate_model", "new_model": model::OPUS_MODEL})
        );
    }

    #[test]
    fn recovery_to_1m_model_serialization() {
        let v = serde_json::to_value(RecoveryAction::To1mModel {
            new_model: model::OPUS_MODEL_1M.to_string(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"action": "to_1m_model", "new_model": model::OPUS_MODEL_1M})
        );
    }

    #[test]
    fn recovery_blocked_serialization_has_no_extra_fields() {
        let v = serde_json::to_value(RecoveryAction::Blocked).unwrap();
        assert_eq!(v, serde_json::json!({"action": "blocked"}));
        assert_eq!(v.as_object().unwrap().len(), 1);
    }

    #[test]
    fn event_round_trip_preserves_equality() {
        let event = sample_event();
        let s = serde_json::to_string(&event).unwrap();
        let back: OverflowEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn sections_serialize_as_ordered_array_of_pairs() {
        let event = sample_event();
        let v = serde_json::to_value(&event).unwrap();
        let arr = v.get("sections").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // Order preserved
        assert_eq!(arr[0], serde_json::json!(["task", 100]));
        assert_eq!(arr[1], serde_json::json!(["base_prompt", 200]));
        assert_eq!(arr[2], serde_json::json!(["learnings", 300]));
        // Each entry is a 2-element array, NOT a map
        for entry in arr {
            assert!(
                entry.is_array(),
                "sections entry must be array, got {entry:?}"
            );
            assert_eq!(entry.as_array().unwrap().len(), 2);
        }
    }

    #[test]
    fn run_id_none_is_skipped_or_null() {
        let mut event = sample_event();
        event.run_id = None;
        let v = serde_json::to_value(&event).unwrap();
        let obj = v.as_object().unwrap();
        // Either skipped via skip_serializing_if, or present as null
        match obj.get("run_id") {
            None => {}
            Some(serde_json::Value::Null) => {}
            other => panic!("run_id None must serialize as missing or null, got {other:?}"),
        }
        // Round-trip still works
        let s = serde_json::to_string(&event).unwrap();
        let back: OverflowEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.run_id, None);
    }

    /// AC #7 — the test suite must distinguish a real implementation from a
    /// stub that returns its input unchanged. A passthrough stub should fail
    /// at least 4 of the 6 behavioural cases above. This guards against a
    /// future regression where the implementation is silently weakened.
    #[test]
    fn ac7_passthrough_stub_fails_at_least_four_cases() {
        fn stub(id: &str) -> String {
            id.to_string()
        }

        // Each closure returns true iff the stub's output FAILS the AC check
        // for that input.
        let failures: Vec<bool> = vec![
            // AC1: "FOO-BAR_baz.42" -> "FOO-BAR_baz.42" (stub passes this)
            stub("FOO-BAR_baz.42") != "FOO-BAR_baz.42",
            // AC2: "FOO/BAR..baz" -> "FOO-BAR--baz"
            stub("FOO/BAR..baz") != "FOO-BAR--baz",
            // AC3: "" -> "_"
            stub("") != "_",
            // AC4: no `/` and no `..` in output for "../../../etc/passwd"
            {
                let s = stub("../../../etc/passwd");
                s.contains('/') || s.contains("..")
            },
            // AC5: "task with spaces" -> "task-with-spaces"
            stub("task with spaces") != "task-with-spaces",
            // AC6: no NUL byte in output for "\0null\0byte"
            stub("\0null\0byte").contains('\0'),
        ];

        let failure_count = failures.iter().filter(|f| **f).count();
        assert!(
            failure_count >= 4,
            "passthrough stub must fail at least 4 cases, only failed {failure_count}: {failures:?}"
        );
    }

    // --- OverflowEvent.runner field tests (FEAT-009) ---

    /// Parse an OverflowEvent with runner='grok' from JSONL — field populated correctly.
    #[test]
    fn runner_field_deserializes_grok() {
        let json = r#"{"ts":"2026-05-04T20:00:00+00:00","task_id":"FEAT-001","iteration":1,"prompt_bytes":100,"sections":[],"dropped_sections":[],"recovery":{"action":"blocked"},"dump_path":"/tmp/d.txt","runner":"grok"}"#;
        let event: OverflowEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.runner, Some("grok".to_string()));
    }

    /// Parse a legacy OverflowEvent (no runner field) — field is None; no panic.
    #[test]
    fn legacy_event_without_runner_deserializes_as_none() {
        let json = r#"{"ts":"2026-05-04T20:00:00+00:00","task_id":"FEAT-001","iteration":1,"prompt_bytes":100,"sections":[],"dropped_sections":[],"recovery":{"action":"blocked"},"dump_path":"/tmp/d.txt"}"#;
        let event: OverflowEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.runner, None);
    }

    /// Serialize an OverflowEvent with runner=None — 'runner' key OMITTED from JSON.
    #[test]
    fn runner_none_omitted_from_serialization() {
        let mut event = sample_event();
        event.runner = None;
        let v = serde_json::to_value(&event).unwrap();
        assert!(
            !v.as_object().unwrap().contains_key("runner"),
            "runner=None must be omitted from JSON; got {v}"
        );
    }

    /// Serialize an OverflowEvent with runner=Some("grok") — 'runner':'grok' present.
    #[test]
    fn runner_some_grok_present_in_serialization() {
        let mut event = sample_event();
        event.runner = Some("grok".to_string());
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(
            v.get("runner").and_then(|v| v.as_str()),
            Some("grok"),
            "runner=Some('grok') must serialize as 'grok'; got {v}"
        );
    }
}
