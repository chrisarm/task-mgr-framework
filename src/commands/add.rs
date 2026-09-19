//! Add a single task from JSON input.
//!
//! `task-mgr add` accepts one task's PRD-shape JSON (via `--json <str>` or
//! stdin), inserts it into the DB, and mirrors it into the active PRD JSON
//! file. Claude never edits the PRD file — only this command does.
//!
//! Priority is auto-computed when the input omits it: the command runs the
//! same selection logic as `task-mgr next`, reads the current top task's
//! priority, and assigns `new_priority = top.priority - 1` (or `0` if the
//! queue is empty), guaranteeing the new task ranks ahead on the next
//! iteration.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::context::{
    choose_cli_write_path, cli_write_path, default_prd_roots, load_known_prefixes, locate_prd_json,
    preflight_from_json_path, refuse_unpinned_write, sole_task_list_path,
};
use crate::commands::init::import::{
    DEPRECATED_RELATIONSHIPS_WARNING, insert_relationship, insert_task, insert_task_file,
    insert_task_relationships, resolve_prd_file_path,
};
use crate::commands::init::parse::PrdUserStory;
use crate::commands::next;
use crate::commands::prd_json::append_user_story;
use crate::output::ui;
use crate::{TaskMgrError, TaskMgrResult};

// Re-export so `commands::add::resolve_context` (main.rs logging) and existing
// test paths keep compiling after the move to `commands::context`.
pub use crate::commands::context::{ResolutionSource, ResolvedContext, resolve_context};

/// Deserialized input for `task-mgr add`.
///
/// Mirrors [`PrdUserStory`] but makes `priority` and `passes` optional so
/// minimal inputs work (priority is auto-computed; absent `passes` means
/// `false` → status `todo`). Anything not supplied here is carried through
/// as the default when the struct is converted into a full `PrdUserStory`.
///
/// **PR-2 CONTRACT-003:** must carry `human_review_outcome: Option<Value>`
/// (JSON `humanReviewOutcome`) and copy it in `into_prd_user_story` so a
/// spawned CLARIFY row does not drop the key. JSON-only — no DB column.
/// Full contract: `## CONTRACT-003` in `tasks/progress-a8855e28.txt`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddTaskInput {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub passes: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub review_scope: Option<Value>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub source_review: Option<String>,
    #[serde(default)]
    pub touches_files: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub synergy_with: Vec<String>,
    #[serde(default)]
    pub batch_with: Vec<String>,
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, rename = "estimatedEffort", alias = "difficulty")]
    pub difficulty: Option<String>,
    #[serde(default)]
    pub escalation_note: Option<String>,
    #[serde(default)]
    pub required_tests: Vec<String>,
    #[serde(default)]
    pub max_retries: Option<i32>,
    #[serde(default)]
    pub requires_human: Option<bool>,
    #[serde(default)]
    pub human_review_timeout: Option<u32>,
    #[serde(default)]
    pub claims_shared_infra: Option<bool>,
    /// CLARIFY human-review resolution payload (CONTRACT-003). JSON-only —
    /// copied into `PrdUserStory` so spawned CLARIFY rows do not drop the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_review_outcome: Option<Value>,
}

impl AddTaskInput {
    /// Task-ID prefix (everything before the first type segment), used to
    /// scope the `next`-based priority computation to sibling tasks.
    ///
    /// Given `CHAIN-FEAT-001` returns `"CHAIN"`; given `FEAT-001` returns
    /// `None` (no recognisable prefix).
    fn task_prefix(&self) -> Option<&str> {
        let first_dash = self.id.find('-')?;
        let prefix = &self.id[..first_dash];
        // A bare type segment like `FEAT` is not a prefix — require that
        // the remainder still contains a dash (e.g., `FEAT-001`).
        if self.id[first_dash + 1..].contains('-') {
            Some(prefix)
        } else {
            None
        }
    }

    fn apply_prefix(&mut self, prefix: &str) {
        let pfx = super::init::prefix_id;
        self.id = pfx(prefix, &self.id);
        self.depends_on = self.depends_on.iter().map(|d| pfx(prefix, d)).collect();
        self.synergy_with = self.synergy_with.iter().map(|s| pfx(prefix, s)).collect();
        self.batch_with = self.batch_with.iter().map(|b| pfx(prefix, b)).collect();
        self.conflicts_with = self.conflicts_with.iter().map(|c| pfx(prefix, c)).collect();
    }

    fn into_prd_user_story(self, priority: i32) -> PrdUserStory {
        PrdUserStory {
            id: self.id,
            title: self.title,
            description: self.description,
            priority,
            passes: self.passes.unwrap_or(false),
            notes: self.notes,
            acceptance_criteria: self.acceptance_criteria,
            review_scope: self.review_scope,
            severity: self.severity,
            source_review: self.source_review,
            touches_files: self.touches_files,
            depends_on: self.depends_on,
            synergy_with: self.synergy_with,
            batch_with: self.batch_with,
            conflicts_with: self.conflicts_with,
            model: self.model,
            difficulty: self.difficulty,
            escalation_note: self.escalation_note,
            required_tests: self.required_tests,
            max_retries: self.max_retries,
            requires_human: self.requires_human,
            human_review_timeout: self.human_review_timeout,
            claims_shared_infra: self.claims_shared_infra,
            human_review_outcome: self.human_review_outcome,
        }
    }
}

/// Result of a successful `task-mgr add`.
#[derive(Debug, Clone, Serialize)]
pub struct AddResult {
    pub task_id: String,
    pub priority: i32,
    pub prd_path: Option<PathBuf>,
    pub priority_source: PrioritySource,
}

/// How the final priority was determined.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrioritySource {
    /// User passed `--priority` on the CLI.
    CliOverride,
    /// Input JSON had a `priority` field.
    Input,
    /// Derived from `select_next_task`'s top pick.
    AutoOneLessThanNext,
    /// Queue was empty when auto-computing.
    AutoEmptyQueue,
}

/// Entry point for `task-mgr add`.
///
/// `db_dir` is the `.task-mgr` directory. `input_json` is a single task's
/// PRD-shape JSON. `priority_override` from the CLI wins over any
/// `priority` field in the input JSON. `from_json` pins an already-registered
/// effort (CONTRACT-002); missing/directory paths fail before input parse.
pub fn add(
    db_dir: &Path,
    input_json: &str,
    priority_override: Option<i32>,
    depended_on_by: &[String],
    from_json: Option<&Path>,
) -> TaskMgrResult<AddResult> {
    // Missing / directory pin checks before parsing input JSON (AC: error
    // before parse). Full registration still runs via resolve_context below
    // (needs the DB) and still precedes any write transaction.
    if let Some(path) = from_json {
        preflight_from_json_path(path, "add")?;
    }

    let input: AddTaskInput = serde_json::from_str(input_json).map_err(|e| {
        TaskMgrError::invalid_state(
            "add",
            "input JSON",
            "valid single-task PRD JSON (fields: id, title, ...)",
            format!("parse error: {e}"),
        )
    })?;

    if input.id.trim().is_empty() {
        return Err(TaskMgrError::invalid_state(
            "add",
            "id",
            "non-empty string",
            "empty",
        ));
    }
    if input.title.trim().is_empty() {
        return Err(TaskMgrError::invalid_state(
            "add",
            "title",
            "non-empty string",
            "empty",
        ));
    }

    let _lock = crate::db::LockGuard::acquire(db_dir)?;
    let conn = crate::db::open_connection(db_dir)?;
    let (source_root, worktree_root) = default_prd_roots(db_dir);

    add_with_conn_in(
        &conn,
        input,
        priority_override,
        depended_on_by,
        from_json,
        Some(&source_root),
        Some(&worktree_root),
    )
}

/// Testable variant that takes an already-open connection (used by unit tests
/// with in-memory DBs).
///
/// `source_root` / `worktree_root` resolve stored `prd_files` paths via
/// [`resolve_prd_file_path`]. Tests may pass the temp project dir for both.
pub fn add_with_conn(
    conn: &Connection,
    input: AddTaskInput,
    priority_override: Option<i32>,
    depended_on_by: &[String],
    from_json: Option<&Path>,
) -> TaskMgrResult<AddResult> {
    add_with_conn_in(
        conn,
        input,
        priority_override,
        depended_on_by,
        from_json,
        None,
        None,
    )
}

fn add_with_conn_in(
    conn: &Connection,
    mut input: AddTaskInput,
    priority_override: Option<i32>,
    depended_on_by: &[String],
    from_json: Option<&Path>,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<AddResult> {
    // Re-validate id/title so callers that bypass `add()` (tests, future
    // internal callers) still get the guarantee.
    if input.id.trim().is_empty() {
        return Err(TaskMgrError::invalid_state(
            "add",
            "id",
            "non-empty string",
            "empty",
        ));
    }
    if input.title.trim().is_empty() {
        return Err(TaskMgrError::invalid_state(
            "add",
            "title",
            "non-empty string",
            "empty",
        ));
    }

    // Auto-prefix / pin: flag → env → single-prefix. Empty prefix (NULL-prefix
    // --from-json pin) skips apply_prefix AND prefix_id.
    let resolved_ctx = crate::commands::context::resolve_context_with_roots(
        conn,
        from_json,
        "add",
        source_root,
        worktree_root,
    )?;

    // Write-only ≥2 refuse (shared with update). Keep OUT of resolve_context.
    refuse_unpinned_write(conn, &resolved_ctx, "add")?;

    // Emit resolved-context line as the FIRST stderr output, before any write
    // or downstream warning. Agents can read stderr line 1 to learn which PRD
    // is active without running a separate `task-mgr current`.
    if let Some(ref ctx) = resolved_ctx {
        let target = if ctx.prd_json_path == PathBuf::new() {
            "(none)".to_string()
        } else {
            ctx.prd_json_path.display().to_string()
        };
        ui::emit(&format!(
            "→ active prefix={}  source={}  target={}",
            ctx.prefix, ctx.source, target,
        ));
    }

    // Hard-refuse cross-PRD `--depended-on-by` before any DB write. Each
    // foreign-prefix target gets a worked refusal naming the foreign PRD's
    // path and both fix commands. Pre-flight runs before the DB transaction
    // opens so a refusal can never leak a stray row or touch the target JSON.
    reject_cross_prd_depended_on_by(conn, depended_on_by, resolved_ctx.as_ref())?;

    let prefixed_depended_on_by: Vec<String>;
    let effective_depended_on_by: &[String] = if let Some(ref ctx) = resolved_ctx {
        let prefix = &ctx.prefix;
        if prefix.is_empty() {
            // NULL-prefix / --no-prefix pin: skip apply_prefix AND prefix_id
            // (otherwise prefix_id("", "FEAT-001") → "-FEAT-001").
            depended_on_by
        } else {
            reject_foreign_prefix(conn, &input.id, &input.depends_on, depended_on_by, prefix)?;
            let original_id = input.id.clone();
            input.apply_prefix(prefix);
            if input.id != original_id {
                ui::emit(&format!(
                    "Note: auto-prefixed task ID as {} (active prefix: {})",
                    input.id, prefix,
                ));
            }
            prefixed_depended_on_by = depended_on_by
                .iter()
                .map(|id| super::init::prefix_id(prefix, id))
                .collect();
            &prefixed_depended_on_by
        }
    } else {
        depended_on_by
    };

    // Pre-flight: reject duplicate IDs before any writes. Propagate DB errors
    // rather than swallowing them — an unexpected schema/I/O error must not
    // be reinterpreted as "no conflict" and fall through to insert_task.
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE id = ?",
        [&input.id],
        |row| row.get(0),
    )?;
    if exists > 0 {
        return Err(TaskMgrError::invalid_state(
            "add",
            "task id",
            "unique",
            format!("{} already exists in database", input.id),
        ));
    }

    // Pre-flight: every --depended-on-by id must exist. Fail BEFORE any write
    // so a typo can't leave the DB with a new task whose reverse links are
    // missing.
    for existing_id in effective_depended_on_by {
        let found: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = ?",
            [existing_id],
            |row| row.get(0),
        )?;
        if found == 0 {
            return Err(TaskMgrError::invalid_state(
                "add",
                "depended-on-by target",
                "existing task id",
                format!("{} not found in database", existing_id),
            ));
        }
    }

    // Resolve priority.
    let (priority, priority_source) = resolve_priority(conn, &input, priority_override);

    // JSON sync strip prefix: active PRD prefix only. Never derive from id
    // shape — CODE-FIX-001 → "CODE" would strip to FIX-001 on NULL-prefix.
    let task_prefix = resolved_ctx
        .as_ref()
        .map(|c| c.prefix.as_str())
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let story = input.into_prd_user_story(priority);

    // Insert task + relationships + files in a single transaction.
    let tx = conn.unchecked_transaction()?;
    insert_task(&tx, &story, None)?;
    let rel_outcome = insert_task_relationships(&tx, &story)?;
    for file_path in &story.touches_files {
        insert_task_file(&tx, &story.id, file_path)?;
    }
    // Reverse links: each `existing_id` now dependsOn the NEW task.
    // Argument order: (task_id=<existing>, related_id=<new>, rel_type="dependsOn").
    for existing_id in effective_depended_on_by {
        insert_relationship(&tx, existing_id, &story.id, "dependsOn")?;
    }
    tx.commit()?;

    if rel_outcome.had_deprecated {
        ui::emit(DEPRECATED_RELATIONSHIPS_WARNING);
    }

    // Best-effort PRD JSON sync. Failure here logs but does not roll back
    // the DB — the task is already in the database (learnings #3440 / #1561).
    // Operator copy names `task-mgr current` and retry `--from-json`, never
    // export. Write path is `ctx.prd_json_path` only (learning #4237) — never
    // re-resolve a task_list row after commit. `--from-json` already stored
    // the canonical flag PATH; default stored remap-then-is_file. `ctx is
    // None` → sync iff exactly one task_list row (remap-then-is_file).
    // Empty write path after a registered row must NOT claim "unregistered".
    let sync_decision = match &resolved_ctx {
        Some(ctx) if ctx.prd_json_path.as_os_str().is_empty() => {
            // Empty stored path: either no task_list row for this prefix, or
            // a row whose remapped/registered paths are not regular files.
            match locate_prd_json(conn, Some(ctx.prefix.as_str())) {
                Ok(Some(_)) => SyncDecision::Skip(JsonSyncSkipReason::RegisteredNotAFile),
                Ok(None) => SyncDecision::Skip(JsonSyncSkipReason::Unregistered),
                Err(e) => SyncDecision::ResolveErr(e),
            }
        }
        Some(ctx) => SyncDecision::Path(ctx.prd_json_path.clone()),
        None => match sole_task_list_path(conn) {
            Ok(Some(registered)) => {
                let path = match (source_root, worktree_root) {
                    (Some(src), Some(wt)) => choose_cli_write_path(&registered, src, wt),
                    _ => cli_write_path(&registered),
                };
                if path.as_os_str().is_empty() {
                    SyncDecision::Skip(JsonSyncSkipReason::RegisteredNotAFile)
                } else {
                    SyncDecision::Path(path)
                }
            }
            Ok(None) => SyncDecision::Skip(JsonSyncSkipReason::Unregistered),
            Err(e) => SyncDecision::ResolveErr(e),
        },
    };
    let prd_path = match sync_decision {
        SyncDecision::Path(path) => match append_user_story(
            &path,
            &story,
            effective_depended_on_by,
            task_prefix.as_deref(),
        ) {
            Ok(()) => Some(path),
            Err(e) => {
                ui::emit_err(&format_json_sync_failure_warning(&story.id, &path, &e));
                Some(path)
            }
        },
        SyncDecision::Skip(reason) => {
            ui::emit(&format_json_sync_skip_note(&story.id, reason));
            None
        }
        SyncDecision::ResolveErr(e) => {
            ui::emit_err(&format_json_sync_resolve_err(&story.id, &e));
            None
        }
    };

    Ok(AddResult {
        task_id: story.id,
        priority,
        prd_path,
        priority_source,
    })
}

/// Resolve the final priority + record which source won.
///
/// Precedence: `--priority` flag > `priority` field in input JSON > auto from
/// `select_next_task`'s top pick. Empty queue → `0`.
fn resolve_priority(
    conn: &Connection,
    input: &AddTaskInput,
    priority_override: Option<i32>,
) -> (i32, PrioritySource) {
    if let Some(p) = priority_override {
        return (p, PrioritySource::CliOverride);
    }
    if let Some(p) = input.priority {
        return (p, PrioritySource::Input);
    }
    match next::select_next_task(conn, &[], input.task_prefix()) {
        Ok(res) => match res.task {
            Some(top) => (
                top.task.priority.saturating_sub(1),
                PrioritySource::AutoOneLessThanNext,
            ),
            None => (0, PrioritySource::AutoEmptyQueue),
        },
        Err(_) => (0, PrioritySource::AutoEmptyQueue),
    }
}

fn foreign_prefix_err(
    field: &str,
    offender: &str,
    foreign: &str,
    active_prefix: &str,
) -> TaskMgrError {
    TaskMgrError::invalid_state(
        "add",
        field,
        format!("IDs with the active prefix '{active_prefix}' or bare IDs"),
        format!(
            "{offender} carries foreign prefix '{foreign}' (active: '{active_prefix}'); \
             pass --from-json or correct the ID"
        ),
    )
}

/// Reject task IDs that carry a known foreign PRD prefix before auto-prefixing
/// runs. Called after `resolve_active_prefix` returns `Some(active)` and before
/// `input.apply_prefix(active)`.
///
/// Foreign prefix = any `task_prefix` in `prd_metadata` that is NOT the active
/// one. The check uses `format!("{foreign}-")` (trailing dash) to avoid the
/// false-positive where prefix 'A' would match 'AB-FEAT-001' via a naive
/// `starts_with("A")`.
///
/// Bare IDs (no recognizable prefix) and already-active-prefixed IDs pass
/// through — `apply_prefix` handles both correctly.
fn reject_foreign_prefix(
    conn: &Connection,
    id: &str,
    depends_on: &[String],
    depended_on_by: &[String],
    active_prefix: &str,
) -> TaskMgrResult<()> {
    let foreign_prefixes: Vec<String> = load_known_prefixes(conn)?
        .into_iter()
        .filter(|p| p.as_str() != active_prefix)
        .collect();
    if foreign_prefixes.is_empty() {
        return Ok(());
    }
    for foreign in &foreign_prefixes {
        let fwd = format!("{foreign}-");
        if id.starts_with(&fwd) {
            return Err(foreign_prefix_err("id", id, foreign, active_prefix));
        }
        for dep in depends_on {
            if dep.starts_with(&fwd) {
                return Err(foreign_prefix_err("dependsOn", dep, foreign, active_prefix));
            }
        }
        for dep in depended_on_by {
            if dep.starts_with(&fwd) {
                return Err(foreign_prefix_err(
                    "depended-on-by",
                    dep,
                    foreign,
                    active_prefix,
                ));
            }
        }
    }
    Ok(())
}

/// Extract the PRD prefix from a task id.
///
/// Returns `Some(prefix)` only when the id has the shape `<prefix>-<type>-<num>`
/// (i.e. contains at least two dashes). Single-segment ids like `MILESTONE-1`
/// return `None` — they're treated as bare ids and will be auto-prefixed.
///
/// Mirrors [`AddTaskInput::task_prefix`] but operates on arbitrary id strings
/// (used for `--depended-on-by` targets that don't live on the input struct).
fn extract_id_prefix(id: &str) -> Option<&str> {
    let first_dash = id.find('-')?;
    let prefix = &id[..first_dash];
    if id[first_dash + 1..].contains('-') {
        Some(prefix)
    } else {
        None
    }
}

/// Hard-refuse cross-PRD `--depended-on-by` before any DB write.
///
/// For each `--depended-on-by` target id, extract its prefix and compare
/// against the active PRD's prefix:
///
/// * Bare id (no recognizable prefix) → skip; auto-prefix path handles it.
/// * Prefix matches the active prefix → OK (same PRD).
/// * Active prefix unset (caller passed `None`) → refuse with a "no active
///   PRD" message so the operator must explicitly opt in to a PRD.
/// * Foreign prefix registered in `prd_metadata` (known OTHER) → refuse with
///   the PRD's actual path AND both fix commands.
/// * Active prefix empty (NULL-prefix / `--no-prefix` pin) and target's first
///   segment is **not** a registered `prd_metadata.task_prefix` → OK.
///   Spawn-fixup bodies like `CODE-REVIEW-1` must not be treated as foreign
///   prefix `CODE` just because empty active ≠ `CODE`.
/// * Non-empty active prefix + unregistered foreign first segment → refuse
///   with a hint pointing at `task-mgr list --prefix <p>`.
///
/// This runs BEFORE `conn.unchecked_transaction()` so a refusal can never
/// leak a stray row or touch the target JSON file. The check only reads
/// `prd_metadata` / `prd_files` (no writes); a refusal is reproducible
/// across retries.
fn reject_cross_prd_depended_on_by(
    conn: &Connection,
    depended_on_by: &[String],
    active_ctx: Option<&ResolvedContext>,
) -> TaskMgrResult<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let source_root = crate::git::main_repo_root_at(&cwd).unwrap_or_else(|| cwd.clone());
    let worktree_root = crate::git::worktree_root().unwrap_or_else(|| source_root.clone());
    for target_id in depended_on_by {
        let Some(target_prefix) = extract_id_prefix(target_id) else {
            continue;
        };
        let Some(ctx) = active_ctx else {
            return Err(TaskMgrError::invalid_state(
                "add",
                "depended-on-by",
                "an active PRD prefix to scope the target",
                format!(
                    "no active PRD; set TASK_MGR_ACTIVE_PREFIX or pass --from-json \
                     (target '{target_id}' carries prefix '{target_prefix}')"
                ),
            ));
        };
        if target_prefix == ctx.prefix {
            continue;
        }
        let foreign_path = locate_prd_json(conn, Some(target_prefix))?
            .map(|stored| resolve_prd_file_path(&stored, &source_root, &worktree_root));
        let active_prefix = &ctx.prefix;
        match foreign_path {
            Some(path) => {
                return Err(TaskMgrError::invalid_state(
                    "add",
                    "depended-on-by",
                    format!("a task id belonging to the active PRD (prefix '{active_prefix}')"),
                    format!(
                        "Refusing: target '{target_id}' lives in PRD {} (prefix {target_prefix}), \
                         but active prefix is {active_prefix}. \
                         Fixes: (a) TASK_MGR_ACTIVE_PREFIX={target_prefix} task-mgr add --stdin \
                         --depended-on-by {target_id} '{{...}}'  (b) task-mgr add --from-json \
                         tasks/<correct-prd>.json --stdin --depended-on-by {target_id} '{{...}}'",
                        path.display(),
                    ),
                ));
            }
            None if active_prefix.is_empty() => {
                // NULL-prefix pin: empty active is not a foreign-prefix match.
                // Only known OTHER registered prefixes refuse (handled above).
                continue;
            }
            None => {
                return Err(TaskMgrError::invalid_state(
                    "add",
                    "depended-on-by",
                    format!("a task id belonging to the active PRD (prefix '{active_prefix}')"),
                    format!(
                        "Refusing: target '{target_id}' carries prefix '{target_prefix}', which is \
                         not registered in any known PRD. Run `task-mgr list --prefix {target_prefix}` \
                         to find the right id."
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Stderr guidance after a best-effort JSON sync miss (US-007 / pin 11).
/// Names `task-mgr current` and retry `--from-json`; never export.
fn json_sync_recovery_hint() -> &'static str {
    "Run `task-mgr current` to inspect the write target, then retry with --from-json <path>"
}

/// Why post-commit JSON sync was skipped (CLI does not invent a path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonSyncSkipReason {
    /// No matching `prd_files` task_list row for the active effort.
    Unregistered,
    /// A task_list row exists, but neither remapped nor registered path is a
    /// regular file (auto-review M1 / CODE-FIX-003).
    RegisteredNotAFile,
}

/// Post-commit sync branch: write path, skip with reason, or resolve error.
enum SyncDecision {
    Path(PathBuf),
    Skip(JsonSyncSkipReason),
    ResolveErr(TaskMgrError),
}

fn format_json_sync_failure_warning(task_id: &str, path: &Path, err: &TaskMgrError) -> String {
    format!(
        "Warning: task {task_id} added to DB but PRD JSON sync failed ({}): {err}. {}",
        path.display(),
        json_sync_recovery_hint(),
    )
}

fn format_json_sync_skip_note(task_id: &str, reason: JsonSyncSkipReason) -> String {
    let why = match reason {
        JsonSyncSkipReason::Unregistered => "no PRD JSON registered in prd_files",
        JsonSyncSkipReason::RegisteredNotAFile => {
            "registered PRD JSON path is not a regular file on disk"
        }
    };
    format!(
        "Note: task {task_id} added to DB; {why} — skipping file sync. {}",
        json_sync_recovery_hint(),
    )
}

fn format_json_sync_resolve_err(task_id: &str, err: &TaskMgrError) -> String {
    format!(
        "Warning: task {task_id} added to DB; could not resolve PRD JSON for sync: {err}. {}",
        json_sync_recovery_hint(),
    )
}

/// Render for `--format text` CLI output.
pub fn format_text(result: &AddResult) -> String {
    let source = match result.priority_source {
        PrioritySource::CliOverride => "cli --priority",
        PrioritySource::Input => "input json",
        PrioritySource::AutoOneLessThanNext => "auto (next.priority - 1)",
        PrioritySource::AutoEmptyQueue => "auto (empty queue → 0)",
    };
    let mut out = format!(
        "Added task {} with priority {} (source: {})",
        result.task_id, result.priority, source,
    );
    if let Some(p) = &result.prd_path {
        out.push_str(&format!("\nSynced into PRD JSON: {}", p.display()));
    } else {
        out.push_str("\nPRD JSON: no file registered — DB-only insert");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::schema::create_schema;

    fn memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    const ACTIVE_PREFIX_ENV: &str = crate::loop_engine::claude::ACTIVE_PREFIX_ENV;

    /// RAII guard: saves and restores `TASK_MGR_ACTIVE_PREFIX` (or removes it)
    /// on drop. `set_var`/`remove_var` are unsafe in current Rust; scoping
    /// them inside this guard keeps the unsafety auditable.
    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }
    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            unsafe { std::env::set_var(name, value) };
            Self { name, prior }
        }
        fn unset(name: &'static str) -> Self {
            let prior = std::env::var(name).ok();
            unsafe { std::env::remove_var(name) };
            Self { name, prior }
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var(self.name, v) },
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }

    /// Compound guard: holds the process-wide mutex AND clears the env var.
    /// Tests that don't care about env-var state but call code that reads it
    /// (e.g. `add_with_conn` → `resolve_active_prefix`) start with this so
    /// they don't observe state from a concurrent env-setting test.
    ///
    /// Field order is load-bearing: Rust drops struct fields in declaration
    /// order, so `_env` MUST come before `_lock`. This ensures the env var is
    /// restored to its outer value *before* the mutex is released — preventing
    /// a race where another thread acquires the lock while `_env` still holds
    /// a stale/mid-test value and subsequently overwrites the new holder's env.
    struct EnvIsolation {
        _env: EnvVarGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    fn isolate_env() -> EnvIsolation {
        let lock = crate::ENV_PREFIX_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let env = EnvVarGuard::unset(ACTIVE_PREFIX_ENV);
        EnvIsolation {
            _env: env,
            _lock: lock,
        }
    }

    fn minimal_input(id: &str) -> AddTaskInput {
        AddTaskInput {
            id: id.to_string(),
            title: "t".to_string(),
            description: None,
            priority: None,
            passes: None,
            notes: None,
            acceptance_criteria: vec![],
            review_scope: None,
            severity: None,
            source_review: None,
            touches_files: vec![],
            depends_on: vec![],
            synergy_with: vec![],
            batch_with: vec![],
            conflicts_with: vec![],
            model: None,
            difficulty: None,
            escalation_note: None,
            required_tests: vec![],
            max_retries: None,
            requires_human: None,
            human_review_timeout: None,
            claims_shared_infra: None,
            human_review_outcome: None,
        }
    }

    #[test]
    fn test_add_task_input_copies_human_review_outcome_into_story() {
        let json = r#"{
            "id": "CLARIFY-001",
            "title": "Confirm floor",
            "requiresHuman": true,
            "humanReviewOutcome": {
                "resolvedAt": "2026-09-18",
                "resolvedBy": "operator",
                "confirmedValues": {"floor": 2},
                "deltasFromProposed": [],
                "additionalRequirements": []
            }
        }"#;
        let input: AddTaskInput = serde_json::from_str(json).expect("deserialize AddTaskInput");
        assert!(
            input.human_review_outcome.is_some(),
            "AddTaskInput must keep humanReviewOutcome"
        );
        let story = input.into_prd_user_story(1);
        let value = serde_json::to_value(&story).expect("serialize story");
        assert!(
            value.get("humanReviewOutcome").is_some(),
            "into_prd_user_story must copy humanReviewOutcome so spawned CLARIFY rows keep it"
        );
        assert_eq!(value["humanReviewOutcome"]["resolvedBy"], "operator");
        assert_eq!(value["humanReviewOutcome"]["confirmedValues"]["floor"], 2);
    }

    #[test]
    fn test_task_prefix_extracts_prefix() {
        let i = minimal_input("CHAIN-FEAT-001");
        assert_eq!(i.task_prefix(), Some("CHAIN"));
    }

    #[test]
    fn test_task_prefix_single_segment_returns_none() {
        let i = minimal_input("FEAT-001");
        assert_eq!(i.task_prefix(), None);
    }

    #[test]
    fn test_task_prefix_no_dash_returns_none() {
        let i = minimal_input("STANDALONE");
        assert_eq!(i.task_prefix(), None);
    }

    #[test]
    fn test_cli_priority_wins_over_input() {
        let conn = memory_db();
        let mut input = minimal_input("X-FEAT-001");
        input.priority = Some(42);
        let (p, src) = resolve_priority(&conn, &input, Some(7));
        assert_eq!(p, 7);
        assert_eq!(src, PrioritySource::CliOverride);
    }

    #[test]
    fn test_input_priority_wins_over_auto() {
        let conn = memory_db();
        let mut input = minimal_input("X-FEAT-001");
        input.priority = Some(42);
        let (p, src) = resolve_priority(&conn, &input, None);
        assert_eq!(p, 42);
        assert_eq!(src, PrioritySource::Input);
    }

    #[test]
    fn test_auto_priority_empty_queue_returns_zero() {
        let conn = memory_db();
        let input = minimal_input("X-FEAT-001");
        let (p, src) = resolve_priority(&conn, &input, None);
        assert_eq!(p, 0);
        assert_eq!(src, PrioritySource::AutoEmptyQueue);
    }

    #[test]
    fn test_auto_priority_one_less_than_top() {
        let conn = memory_db();
        // Seed one todo task at priority 10.
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('X-SEED-001', 't', 10, 'todo')",
            [],
        )
        .unwrap();
        let input = minimal_input("X-NEW-001");
        let (p, src) = resolve_priority(&conn, &input, None);
        assert_eq!(
            p, 9,
            "new task should rank one priority point ahead of the current top"
        );
        assert_eq!(src, PrioritySource::AutoOneLessThanNext);
    }

    #[test]
    fn test_add_rejects_duplicate_id() {
        let _iso = isolate_env();
        let conn = memory_db();
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES ('X-SEED-001', 't', 10, 'todo')",
            [],
        )
        .unwrap();
        let input = minimal_input("X-SEED-001");
        let err = add_with_conn(&conn, input, None, &[], None).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("X-SEED-001"),
            "error must mention the id: {msg}"
        );
    }

    #[test]
    fn test_add_empty_id_rejected() {
        // Empty id is rejected by `add()` (the public entry) via the
        // early validate — exercise that path by going through add_with_conn
        // with a manual bypass is awkward, so just verify the check in add().
        let conn = memory_db();
        let mut input = minimal_input("ignored");
        input.id = "".to_string();
        // add_with_conn doesn't re-validate id; the public add() does.
        // Assert insert_task would fail on empty id (PK constraint) or the
        // duplicate check succeeds trivially. We mainly exercise the format
        // of the public API in an integration test; here just sanity-check
        // the `add()` entry path produces a useful error message via json.
        let err = add(
            std::path::Path::new("/tmp/does-not-exist-task-mgr-add-test"),
            "{\"id\":\"\",\"title\":\"x\"}",
            None,
            &[],
            None,
        );
        assert!(err.is_err());
        // And drop conn silently.
        drop(conn);
    }

    #[test]
    fn test_add_writes_task_into_database() {
        let _iso = isolate_env();
        let conn = memory_db();
        let input = minimal_input("X-FEAT-001");
        let res = add_with_conn(&conn, input, None, &[], None).unwrap();
        assert_eq!(res.task_id, "X-FEAT-001");
        assert_eq!(res.priority_source, PrioritySource::AutoEmptyQueue);

        let (title, priority, status): (String, i32, String) = conn
            .query_row(
                "SELECT title, priority, status FROM tasks WHERE id = 'X-FEAT-001'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "t");
        assert_eq!(priority, 0);
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_add_writes_relationships_and_files() {
        let _iso = isolate_env();
        let conn = memory_db();
        // Seed the dependency so insert_task_relationships can reference it.
        // (The FK constraint on task_relationships doesn't validate target
        // existence, so this is just for realism.)
        let mut input = minimal_input("X-FEAT-002");
        input.depends_on = vec!["X-FEAT-001".to_string()];
        input.touches_files = vec!["src/foo.rs".to_string(), "src/bar.rs".to_string()];
        add_with_conn(&conn, input, None, &[], None).unwrap();

        let rel_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'X-FEAT-002' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rel_count, 1);

        let file_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_files WHERE task_id = 'X-FEAT-002'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(file_count, 2);
    }

    // Append unit tests live in `prd_json.rs` (FEAT-003 move).

    fn seed_task(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES (?, 't', 50, 'todo')",
            [id],
        )
        .unwrap();
    }

    #[test]
    fn test_depended_on_by_inserts_reverse_relationship() {
        let _iso = isolate_env();
        let conn = memory_db();
        seed_task(&conn, "MILESTONE-1");

        let input = minimal_input("NEW-001");
        add_with_conn(&conn, input, None, &["MILESTONE-1".to_string()], None).unwrap();

        // Reverse row: MILESTONE-1 (existing) dependsOn NEW-001 (new).
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships \
                 WHERE task_id = 'MILESTONE-1' AND related_id = 'NEW-001' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 1, "reverse dependsOn must be recorded");

        // Sanity: the NEW task's OWN dependsOn is NOT populated from the flag.
        let forward_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships \
                 WHERE task_id = 'NEW-001' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            forward_count, 0,
            "new task must NOT gain forward dependsOn from --depended-on-by"
        );
    }

    #[test]
    fn test_depended_on_by_invalid_id_rejects_before_insert() {
        let _iso = isolate_env();
        let conn = memory_db();
        // Do NOT seed NONEXISTENT-ID.
        let input = minimal_input("NEW-002");
        let err =
            add_with_conn(&conn, input, None, &["NONEXISTENT-ID".to_string()], None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("NONEXISTENT-ID"),
            "error must name the missing id: {msg}"
        );

        // Fail-fast guarantee: new task must NOT be in the DB.
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id = 'NEW-002'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            task_count, 0,
            "new task must not be inserted when --depended-on-by target is missing"
        );
    }

    #[test]
    fn test_depended_on_by_multiple_targets_all_wired() {
        let _iso = isolate_env();
        let conn = memory_db();
        seed_task(&conn, "TARGET-A");
        seed_task(&conn, "TARGET-B");

        let input = minimal_input("NEW-003");
        add_with_conn(
            &conn,
            input,
            None,
            &["TARGET-A".to_string(), "TARGET-B".to_string()],
            None,
        )
        .unwrap();

        for target in ["TARGET-A", "TARGET-B"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM task_relationships \
                     WHERE task_id = ? AND related_id = 'NEW-003' AND rel_type = 'dependsOn'",
                    [target],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{target} must dependOn NEW-003");
        }
    }

    // Resolver env/prefix tests live in `context.rs` (FEAT-002 move). Keep
    // seed_prefix here for write-policy / foreign-prefix tests below.

    fn seed_prefix(conn: &Connection, id: i64, project: &str, task_prefix: &str) {
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (?, ?, ?)",
            rusqlite::params![id, project, task_prefix],
        )
        .unwrap();
    }

    // --- FEAT-003: reject_foreign_prefix ---

    #[test]
    fn test_reject_foreign_prefix_idempotent_active_prefix_ok() {
        // active='A', input already carries the active prefix → Ok (idempotent path).
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        reject_foreign_prefix(&conn, "A-FEAT-001", &[], &[], "A").unwrap();
    }

    #[test]
    fn test_reject_foreign_prefix_bare_id_ok() {
        // active='A', bare ID (no recognizable prefix) → Ok (apply_prefix will handle it).
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        reject_foreign_prefix(&conn, "FEAT-001", &[], &[], "A").unwrap();
    }

    #[test]
    fn test_reject_foreign_prefix_known_foreign_id_errors_with_all_components() {
        // active='A', input='B-FEAT-001' where B is in prd_metadata → Err with
        // all 5 required message components: field name, offending ID, foreign
        // prefix, active prefix, and actionable hint.
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        let err = reject_foreign_prefix(&conn, "B-FEAT-001", &[], &[], "A").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("id"), "must name the field: {msg}");
        assert!(
            msg.contains("B-FEAT-001"),
            "must name the offending ID: {msg}"
        );
        assert!(msg.contains("'B'"), "must name the foreign prefix: {msg}");
        assert!(msg.contains("'A'"), "must name the active prefix: {msg}");
        assert!(
            msg.contains("--from-json") || msg.contains("correct the ID"),
            "must include actionable hint: {msg}"
        );
    }

    #[test]
    fn test_reject_foreign_prefix_depends_on_foreign_errors_naming_field() {
        // active='A', depends_on contains a foreign-prefix ID → Err naming 'dependsOn'.
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        let err = reject_foreign_prefix(&conn, "FEAT-001", &["B-FEAT-1".to_string()], &[], "A")
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("dependsOn"),
            "must name the 'dependsOn' field: {msg}"
        );
        assert!(
            msg.contains("B-FEAT-1"),
            "must name the offending ID: {msg}"
        );
    }

    #[test]
    fn test_reject_foreign_prefix_depended_on_by_foreign_errors_naming_field() {
        // active='A', depended_on_by contains a foreign-prefix ID → Err naming 'depended-on-by'.
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        let err = reject_foreign_prefix(&conn, "FEAT-001", &[], &["B-FEAT-1".to_string()], "A")
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("depended-on-by"),
            "must name the 'depended-on-by' field: {msg}"
        );
        assert!(
            msg.contains("B-FEAT-1"),
            "must name the offending ID: {msg}"
        );
    }

    #[test]
    fn test_reject_foreign_prefix_ab_active_a_input_errors() {
        // active='AB', input='A-FEAT-001' (both AB and A in metadata) → Err.
        // Foreign prefix 'A' matches 'A-FEAT-001' via trailing-dash check ('A-').
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "AB");
        seed_prefix(&conn, 2, "beta", "A");
        let err = reject_foreign_prefix(&conn, "A-FEAT-001", &[], &[], "AB").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("'A'"), "must name foreign prefix 'A': {msg}");
    }

    #[test]
    fn test_reject_foreign_prefix_trailing_dash_discriminator() {
        // active='A', input='AB-FEAT-001' — only the active prefix 'A' is in
        // prd_metadata (no 'AB'). Foreign set is empty → Ok.
        //
        // This is the trailing-dash discriminator: a naive starts_with("A") (no
        // dash) on a hypothetical foreign check for 'A' would falsely match
        // 'AB-FEAT-001'. With the trailing dash: starts_with("A-") = FALSE.
        // The correct implementation avoids this by (a) excluding the active
        // prefix from foreign candidates and (b) using the trailing-dash form.
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        // No 'AB' in prd_metadata → foreign = [] → no checks → Ok.
        reject_foreign_prefix(&conn, "AB-FEAT-001", &[], &[], "A").unwrap();
    }

    #[test]
    fn test_reject_foreign_prefix_known_bad_no_trailing_dash_would_fail_discriminator() {
        // Known-bad guard: a naive starts_with(foreign) WITHOUT trailing dash
        // would false-positive on 'AB-FEAT-001' when foreign='A'.
        // This test uses active='AB' and foreign=['A'], then checks 'AB-FEAT-001'
        // (an active-prefixed ID) does NOT get rejected — a naive no-dash
        // implementation would incorrectly reject it.
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "AB");
        seed_prefix(&conn, 2, "beta", "A");
        // 'AB-FEAT-001' belongs to active 'AB'; foreign 'A' must NOT match it
        // because 'AB-FEAT-001'.starts_with("A-") = FALSE (trailing dash saves us).
        reject_foreign_prefix(&conn, "AB-FEAT-001", &[], &[], "AB").unwrap();
    }

    #[test]
    fn test_reject_foreign_prefix_only_active_prefix_no_rejection() {
        // prd_metadata has only the active prefix → foreign set empty → all inputs pass.
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        // No foreign prefixes → Ok for any input.
        reject_foreign_prefix(&conn, "B-FEAT-001", &[], &[], "A").unwrap();
    }

    #[test]
    fn test_reject_foreign_prefix_integration_end_to_end() {
        // Integration: bare ID + bare dependsOn under active env var produces
        // correctly-prefixed row → verifies FEAT-002 + FEAT-003 + apply_prefix chain.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "A");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");

        let mut input = minimal_input("FIX-001");
        input.depends_on = vec!["OTHER-1".to_string()];
        let res = add_with_conn(&conn, input, None, &[], None).unwrap();

        assert_eq!(res.task_id, "A-FIX-001", "task ID must be auto-prefixed");

        // Verify the relationship was recorded with the prefixed dep ID.
        let rel_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships \
                 WHERE task_id = 'A-FIX-001' AND related_id = 'A-OTHER-1' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rel_count, 1, "dependsOn must be recorded with prefixed ID");
    }

    // --- FEAT-004: extract_id_prefix + reject_cross_prd_depended_on_by ---

    #[test]
    fn extract_id_prefix_returns_first_segment_when_two_dashes() {
        assert_eq!(
            extract_id_prefix("5ba153a7-MILESTONE-FINAL"),
            Some("5ba153a7")
        );
        assert_eq!(extract_id_prefix("alpha-FEAT-001"), Some("alpha"));
    }

    #[test]
    fn extract_id_prefix_returns_none_for_single_segment_ids() {
        assert_eq!(extract_id_prefix("MILESTONE-1"), None);
        assert_eq!(extract_id_prefix("STANDALONE"), None);
        assert_eq!(extract_id_prefix(""), None);
    }

    /// Build a `ResolvedContext` for tests without needing the DB lookup
    /// machinery — write path is ctx.prd_json_path only (no post-commit locate).
    fn fake_ctx(prefix: &str, prd_path: &str) -> ResolvedContext {
        ResolvedContext {
            prefix: prefix.to_string(),
            source: ResolutionSource::EnvVar,
            prd_json_path: PathBuf::from(prd_path),
        }
    }

    #[test]
    fn cross_prd_check_no_active_ctx_with_prefixed_target_refuses() {
        let conn = memory_db();
        // Active context is None → any prefixed target refuses.
        let err = reject_cross_prd_depended_on_by(&conn, &["X-MILESTONE-FINAL".to_string()], None)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no active PRD"), "{msg}");
        assert!(msg.contains("TASK_MGR_ACTIVE_PREFIX"), "{msg}");
        assert!(msg.contains("--from-json"), "{msg}");
    }

    #[test]
    fn cross_prd_check_no_active_ctx_bare_target_ok() {
        let conn = memory_db();
        // Bare id (no recognizable prefix) is fine even without an active context.
        reject_cross_prd_depended_on_by(&conn, &["MILESTONE-1".to_string()], None).unwrap();
    }

    #[test]
    fn cross_prd_check_same_prefix_passes() {
        let conn = memory_db();
        let ctx = fake_ctx("alpha", "");
        reject_cross_prd_depended_on_by(&conn, &["alpha-MILESTONE-FINAL".to_string()], Some(&ctx))
            .unwrap();
    }

    #[test]
    fn cross_prd_check_known_foreign_prd_refuses_with_path_and_both_fixes() {
        let conn = memory_db();
        seed_prefix(&conn, 1, "active-project", "other-prefix");
        seed_prefix(&conn, 2, "foreign-project", "5ba153a7");
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (2, ?, 'task_list')",
            ["/tmp/foreign-prd.json"],
        )
        .unwrap();
        let ctx = fake_ctx("other-prefix", "/tmp/active.json");

        let err = reject_cross_prd_depended_on_by(
            &conn,
            &["5ba153a7-MILESTONE-FINAL".to_string()],
            Some(&ctx),
        )
        .unwrap_err();
        let msg = format!("{err}");

        // Required substrings from acceptance criteria:
        assert!(msg.contains("Refusing:"), "{msg}");
        assert!(msg.contains("5ba153a7-MILESTONE-FINAL"), "{msg}");
        assert!(msg.contains("/tmp/foreign-prd.json"), "{msg}");
        assert!(msg.contains("(prefix 5ba153a7)"), "{msg}");
        assert!(msg.contains("but active prefix is other-prefix"), "{msg}");
        // Fix (a): env-var-and-rerun command.
        assert!(
            msg.contains("TASK_MGR_ACTIVE_PREFIX=5ba153a7 task-mgr add"),
            "fix (a) missing: {msg}"
        );
        // Fix (b): --from-json reference.
        assert!(
            msg.contains("--from-json tasks/<correct-prd>.json"),
            "fix (b) missing: {msg}"
        );
    }

    #[test]
    fn cross_prd_check_unknown_foreign_prefix_refuses_with_list_hint() {
        let conn = memory_db();
        seed_prefix(&conn, 1, "active-project", "alpha");
        // 'unknown' is NOT registered.
        let ctx = fake_ctx("alpha", "");

        let err =
            reject_cross_prd_depended_on_by(&conn, &["unknown-FEAT-001".to_string()], Some(&ctx))
                .unwrap_err();
        let msg = format!("{err}");

        assert!(msg.contains("Refusing:"), "{msg}");
        assert!(msg.contains("unknown-FEAT-001"), "{msg}");
        assert!(
            msg.contains("not registered in any known PRD"),
            "must explain absence: {msg}"
        );
        assert!(
            msg.contains("task-mgr list --prefix unknown"),
            "must suggest list command: {msg}"
        );
    }

    #[test]
    fn cross_prd_check_empty_depended_on_by_ok() {
        let conn = memory_db();
        let ctx = fake_ctx("alpha", "");
        reject_cross_prd_depended_on_by(&conn, &[], Some(&ctx)).unwrap();
        reject_cross_prd_depended_on_by(&conn, &[], None).unwrap();
    }

    #[test]
    fn cross_prd_check_runs_before_db_writes_no_partial_state() {
        // End-to-end via add_with_conn: a foreign-prefix --depended-on-by must
        // refuse with NO new rows in the tasks table and NO modification to
        // the target PRD JSON file's mtime.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "other-prefix");
        let conn = memory_db();
        seed_prefix(&conn, 1, "active-project", "other-prefix");
        seed_prefix(&conn, 2, "foreign-project", "5ba153a7");

        // Real on-disk PRD JSON so we can check mtime + content invariance.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial = r#"{"userStories":[]}"#;
        std::fs::write(tmp.path(), initial).unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (2, ?, 'task_list')",
            [tmp.path().to_str().unwrap()],
        )
        .unwrap();

        let mtime_before = std::fs::metadata(tmp.path()).unwrap().modified().unwrap();
        let rows_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();

        let input = minimal_input("NEW-FIX-001");
        let err = add_with_conn(
            &conn,
            input,
            None,
            &["5ba153a7-MILESTONE-FINAL".to_string()],
            None,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Refusing:"), "{msg}");

        // Row count invariant: no stray row.
        let rows_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows_before, rows_after, "no DB row may be added on refusal");

        // File mtime + content invariant: target JSON untouched.
        let mtime_after = std::fs::metadata(tmp.path()).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "target JSON mtime must not change on refusal",
        );
        let content_after = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(content_after, initial, "target JSON content unchanged");
    }

    #[test]
    fn cross_prd_check_fires_before_duplicate_id_check() {
        // Known-bad guard: an impl that checked DB duplicate-id FIRST would
        // surface "X already exists" instead of "Refusing:" for this input.
        // Set up a state where BOTH checks would fail; assert the cross-PRD
        // refusal wins so we know the order of pre-flight checks is correct.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "alpha");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha-project", "alpha");
        seed_prefix(&conn, 2, "beta-project", "beta");
        // Pre-seed the would-be auto-prefixed id so the dup-id check WOULD fire.
        seed_task(&conn, "alpha-NEW-001");

        let input = minimal_input("NEW-001");
        let err = add_with_conn(
            &conn,
            input,
            None,
            &["beta-MILESTONE-FINAL".to_string()],
            None,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Refusing:"),
            "cross-PRD check must fire first; got: {msg}"
        );
        assert!(
            !msg.contains("already exists"),
            "duplicate-id error must NOT win: {msg}"
        );
    }

    #[test]
    fn cross_prd_check_same_prefix_does_not_regress_existing_behavior() {
        // Regression check: same-PRD --depended-on-by (the common case) still
        // works after the new pre-flight is in place.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "5ba153a7");
        let conn = memory_db();
        seed_prefix(&conn, 1, "p", "5ba153a7");
        seed_task(&conn, "5ba153a7-MILESTONE-FINAL");

        let input = minimal_input("NEW-FIX-001");
        let res = add_with_conn(
            &conn,
            input,
            None,
            &["5ba153a7-MILESTONE-FINAL".to_string()],
            None,
        );
        assert!(
            res.is_ok(),
            "same-PRD add must still succeed: {:?}",
            res.err()
        );
        let r = res.unwrap();
        assert_eq!(r.task_id, "5ba153a7-NEW-FIX-001");
    }

    // --- FEAT-004: NULL-prefix --from-json skips apply_prefix AND prefix_id ---

    #[test]
    fn test_from_json_null_prefix_skips_apply_prefix_and_prefix_id() {
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("noprefix.json");
        std::fs::write(
            &file,
            r#"{"project":"p","userStories":[{"id":"SEED-001","title":"s","priority":10,"passes":false}]}"#,
        )
        .unwrap();

        let conn = memory_db();
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'p', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, ?, 'task_list')",
            [file.to_str().unwrap()],
        )
        .unwrap();
        seed_task(&conn, "SEED-001");

        let input = minimal_input("FEAT-001");
        let res =
            add_with_conn(&conn, input, None, &["SEED-001".to_string()], Some(&file)).unwrap();

        assert_eq!(
            res.task_id, "FEAT-001",
            "NULL-prefix pin must NOT produce -FEAT-001"
        );
        assert!(
            !res.task_id.starts_with('-'),
            "must not call prefix_id(\"\", …)"
        );

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'FEAT-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Reverse link uses the bare depended-on-by id (no prefix_id).
        let rel: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships \
                 WHERE task_id = 'SEED-001' AND related_id = 'FEAT-001' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rel, 1);

        // Appended into the pinned file (canonical flag path).
        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let stories = updated["userStories"].as_array().unwrap();
        assert!(
            stories.iter().any(|s| s["id"].as_str() == Some("FEAT-001")),
            "userStories must gain FEAT-001"
        );
    }

    // --- CODE-FIX-002: NULL-prefix must not strip 3-segment spawn-fixup ids ---

    #[test]
    fn test_from_json_null_prefix_preserves_three_segment_ids() {
        // H1: CODE-FIX-001 on a NULL-prefix pin must stay CODE-FIX-001 in both
        // DB and JSON (never FIX-001 from id-shape "CODE"). --depended-on-by
        // CODE-REVIEW-1 must not refuse as foreign prefix CODE.
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("noprefix.json");
        std::fs::write(
            &file,
            r#"{"project":"p","userStories":[{"id":"CODE-REVIEW-1","title":"r","priority":10,"passes":false}]}"#,
        )
        .unwrap();

        let conn = memory_db();
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'p', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, ?, 'task_list')",
            [file.to_str().unwrap()],
        )
        .unwrap();
        seed_task(&conn, "CODE-REVIEW-1");

        let input = minimal_input("CODE-FIX-001");
        let res = add_with_conn(
            &conn,
            input,
            None,
            &["CODE-REVIEW-1".to_string()],
            Some(&file),
        )
        .expect("NULL-prefix pin must accept CODE-REVIEW-1 depended-on-by");

        assert_eq!(
            res.task_id, "CODE-FIX-001",
            "DB id must stay unprefixed CODE-FIX-001"
        );

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'CODE-FIX-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let rel: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships \
                 WHERE task_id = 'CODE-REVIEW-1' AND related_id = 'CODE-FIX-001' \
                   AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rel, 1, "reverse dependsOn link must land");

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let stories = updated["userStories"].as_array().unwrap();
        assert!(
            stories
                .iter()
                .any(|s| s["id"].as_str() == Some("CODE-FIX-001")),
            "JSON id must be CODE-FIX-001, not FIX-001; got: {updated}"
        );
        assert!(
            !stories.iter().any(|s| s["id"].as_str() == Some("FIX-001")),
            "must not strip id-shape CODE from CODE-FIX-001"
        );
        // Reverse link on the review entry uses the unstripped new id.
        let review = stories
            .iter()
            .find(|s| s["id"].as_str() == Some("CODE-REVIEW-1"))
            .expect("CODE-REVIEW-1 stays in JSON");
        let deps = review["dependsOn"].as_array().unwrap();
        assert!(
            deps.iter().any(|d| d.as_str() == Some("CODE-FIX-001")),
            "dependsOn must reference CODE-FIX-001: {review}"
        );
    }

    #[test]
    fn test_from_json_prefixed_pin_still_strips_to_body_in_json() {
        // Prefixed 8-hex pins stay unchanged: DB gets PREFIX-CODE-FIX-001,
        // JSON writes the unprefixed body CODE-FIX-001.
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("prefixed.json");
        std::fs::write(
            &file,
            r#"{"project":"p","taskPrefix":"a410d276","userStories":[{"id":"SEED-001","title":"s","priority":10,"passes":false}]}"#,
        )
        .unwrap();

        let conn = memory_db();
        seed_prefix(&conn, 1, "p", "a410d276");
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, ?, 'task_list')",
            [file.to_str().unwrap()],
        )
        .unwrap();
        seed_task(&conn, "a410d276-SEED-001");

        let input = minimal_input("CODE-FIX-001");
        let res =
            add_with_conn(&conn, input, None, &["SEED-001".to_string()], Some(&file)).unwrap();

        assert_eq!(res.task_id, "a410d276-CODE-FIX-001");

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let stories = updated["userStories"].as_array().unwrap();
        assert!(
            stories
                .iter()
                .any(|s| s["id"].as_str() == Some("CODE-FIX-001")),
            "prefixed pin must strip to CODE-FIX-001 in JSON; got: {updated}"
        );
        assert!(
            !stories
                .iter()
                .any(|s| s["id"].as_str() == Some("a410d276-CODE-FIX-001")),
            "JSON must not keep the DB-prefixed id"
        );
    }

    #[test]
    fn cross_prd_check_null_prefix_allows_unregistered_three_segment_target() {
        let conn = memory_db();
        let ctx = fake_ctx("", "/tmp/noprefix.json");
        // CODE is not a registered task_prefix — empty active must not treat
        // it as a foreign-prefix mismatch.
        reject_cross_prd_depended_on_by(&conn, &["CODE-REVIEW-1".to_string()], Some(&ctx)).unwrap();
    }

    #[test]
    fn cross_prd_check_null_prefix_still_refuses_known_other_prefix() {
        let conn = memory_db();
        seed_prefix(&conn, 1, "foreign-project", "5ba153a7");
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, ?, 'task_list')",
            ["/tmp/foreign-prd.json"],
        )
        .unwrap();
        let ctx = fake_ctx("", "/tmp/noprefix.json");

        let err = reject_cross_prd_depended_on_by(
            &conn,
            &["5ba153a7-MILESTONE-FINAL".to_string()],
            Some(&ctx),
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Refusing:"), "{msg}");
        assert!(msg.contains("/tmp/foreign-prd.json"), "{msg}");
    }

    // --- FEAT-006: write-only ≥2-prefix refuse (add and update; resolver stays Ok(None)) ---

    #[test]
    fn test_add_refuses_unpinned_multi_prefix_no_db_row() {
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");

        // Resolver probe still Ok(None) — refuse is write-policy only.
        let ctx = resolve_context(&conn, None, "add").unwrap();
        assert!(ctx.is_none(), "resolver must stay Ok(None) for 2+ prefixes");

        let err = add_with_conn(&conn, minimal_input("FIX-001"), None, &[], None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--from-json"),
            "refuse must name --from-json: {msg}"
        );
        assert!(
            msg.contains("TASK_MGR_ACTIVE_PREFIX"),
            "refuse must name TASK_MGR_ACTIVE_PREFIX: {msg}"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id = 'FIX-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "refused add must not insert a DB row");
    }

    #[test]
    fn test_add_zero_prefixes_still_inserts() {
        // PrefixMode::Disabled / programmatic DB: ctx None AND known.len()==0
        // → insert OK (known-bad: if ctx.is_none() { refuse } alone).
        let _iso = isolate_env();
        let conn = memory_db();
        assert!(load_known_prefixes(&conn).unwrap().is_empty());
        assert!(resolve_context(&conn, None, "add").unwrap().is_none());

        let res = add_with_conn(&conn, minimal_input("ZERO-001"), None, &[], None).unwrap();
        assert_eq!(res.task_id, "ZERO-001");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'ZERO-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_add_env_pin_among_multi_prefix_proceeds() {
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "A");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");

        let res = add_with_conn(&conn, minimal_input("FIX-001"), None, &[], None).unwrap();
        assert_eq!(res.task_id, "A-FIX-001");
    }

    #[test]
    fn test_add_depended_on_by_without_pin_multi_prefix_refuses() {
        // Pin 1: --depended-on-by cannot pin; ≥2 prefixes without flag/env →
        // same refuse as no-pin (before any reverse-link write).
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        seed_task(&conn, "B-MILESTONE-1");

        let err = add_with_conn(
            &conn,
            minimal_input("FIX-001"),
            None,
            &["B-MILESTONE-1".to_string()],
            None,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--from-json"), "{msg}");
        assert!(msg.contains("TASK_MGR_ACTIVE_PREFIX"), "{msg}");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id = 'FIX-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "must refuse before insert");
    }

    // --- FEAT-007: JSON-sync failure / skip copy (current + --from-json, never export) ---

    #[test]
    fn add_rs_source_omits_task_mgr_export() {
        // Build the needle without a contiguous literal so this assert itself
        // does not create a false-positive grep hit.
        let needle = format!("{} {}", "task-mgr", "export");
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands/add.rs"));
        assert!(
            !src.contains(&needle),
            "add.rs comment/warning must not mention {needle}"
        );
    }

    fn assert_json_sync_copy_contract(msg: &str) {
        assert!(
            msg.contains("task-mgr current"),
            "must name task-mgr current: {msg}"
        );
        assert!(
            msg.contains("--from-json"),
            "must name retry --from-json: {msg}"
        );
        assert!(!msg.contains("export"), "must not mention export: {msg}");
        assert!(
            !msg.contains("locate_prd_json"),
            "must not name locate_prd_json as the write site: {msg}"
        );
    }

    #[test]
    fn json_sync_failure_warning_names_current_and_from_json_not_export() {
        let err = TaskMgrError::io_error(
            "/tmp/missing.json".to_string(),
            "reading PRD file",
            std::io::Error::new(std::io::ErrorKind::NotFound, "No such file or directory"),
        );
        let msg = format_json_sync_failure_warning("FIX-001", Path::new("/tmp/missing.json"), &err);
        assert!(msg.contains("PRD JSON sync failed"), "{msg}");
        assert!(msg.contains("FIX-001"), "{msg}");
        assert!(msg.contains("/tmp/missing.json"), "{msg}");
        assert_json_sync_copy_contract(&msg);
    }

    #[test]
    fn json_sync_skip_note_names_current_and_from_json_not_export() {
        let msg = format_json_sync_skip_note("MULTI-001", JsonSyncSkipReason::Unregistered);
        assert!(msg.contains("skipping file sync"), "{msg}");
        assert!(msg.contains("MULTI-001"), "{msg}");
        assert!(
            msg.contains("no PRD JSON registered in prd_files"),
            "unregistered reason: {msg}"
        );
        assert_json_sync_copy_contract(&msg);
    }

    #[test]
    fn json_sync_skip_note_registered_not_a_file_does_not_claim_unregistered() {
        let msg = format_json_sync_skip_note("MISS-001", JsonSyncSkipReason::RegisteredNotAFile);
        assert!(msg.contains("skipping file sync"), "{msg}");
        assert!(msg.contains("MISS-001"), "{msg}");
        assert!(
            msg.contains("registered PRD JSON path is not a regular file"),
            "must explain missing/non-file registered path: {msg}"
        );
        assert!(
            !msg.contains("no PRD JSON registered"),
            "must not claim prd_files unregistered when a row exists: {msg}"
        );
        assert_json_sync_copy_contract(&msg);
    }

    #[test]
    fn json_sync_resolve_err_names_current_and_from_json_not_export() {
        let err = TaskMgrError::invalid_state("add", "prd_files", "readable", "db error");
        let msg = format_json_sync_resolve_err("ZERO-001", &err);
        assert!(msg.contains("could not resolve PRD JSON"), "{msg}");
        assert_json_sync_copy_contract(&msg);
    }

    #[test]
    fn json_sync_failure_on_ctx_prd_json_path_does_not_roll_back_db() {
        // Learning #3440: DB commit first; append Err leaves the row.
        // Use --from-json so ctx.prd_json_path is the flag PATH even when the
        // file content cannot be parsed (cli_write_path would empty a missing
        // default path and hit the skip note instead of sync Err).
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("bad.json");
        std::fs::write(&file, "not-valid-json").unwrap();

        let conn = memory_db();
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'p', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, ?, 'task_list')",
            [file.to_str().unwrap()],
        )
        .unwrap();

        let res = add_with_conn(&conn, minimal_input("FIX-001"), None, &[], Some(&file)).unwrap();
        assert_eq!(res.task_id, "FIX-001");
        assert_eq!(
            res.prd_path.as_deref(),
            Some(file.as_path()),
            "failed sync still reports the write target path"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id = 'FIX-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "sync Err must not roll back the DB commit");
    }

    #[test]
    fn json_sync_skip_when_ctx_none_still_commits_db() {
        // ctx is None + zero task_list rows → skip note path; DB insert OK.
        let _iso = isolate_env();
        let conn = memory_db();
        assert!(resolve_context(&conn, None, "add").unwrap().is_none());

        let res = add_with_conn(&conn, minimal_input("SKIP-001"), None, &[], None).unwrap();
        assert_eq!(res.task_id, "SKIP-001");
        assert!(res.prd_path.is_none());

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'SKIP-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "skip note path must not roll back the DB commit");
    }

    #[test]
    fn json_sync_skip_when_registered_path_missing_still_commits_db() {
        // Single prefix + prd_files row pointing at a missing path: CLI skip
        // (no invent) is correct; DB insert must still commit (M1 / CODE-FIX-003).
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("gone.json");

        let conn = memory_db();
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (1, 'p', 'MISS')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (1, ?, 'task_list')",
            [missing.to_str().unwrap()],
        )
        .unwrap();

        let ctx = resolve_context(&conn, None, "add")
            .unwrap()
            .expect("single prefix");
        assert!(
            ctx.prd_json_path.as_os_str().is_empty(),
            "missing registered path must store empty write target"
        );

        let res = add_with_conn(&conn, minimal_input("FIX-001"), None, &[], None).unwrap();
        assert_eq!(res.task_id, "MISS-FIX-001");
        assert!(res.prd_path.is_none(), "must not invent a sync path");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = 'MISS-FIX-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "registered-not-a-file skip must not roll back DB");
    }
}
