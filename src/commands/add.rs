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

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::init::import::{
    DEPRECATED_RELATIONSHIPS_WARNING, insert_relationship, insert_task, insert_task_file,
    insert_task_relationships,
};
use crate::commands::init::parse::PrdUserStory;
use crate::commands::next;
use crate::loop_engine::output_parsing::strip_task_prefix;
use crate::output::ui;
use crate::{TaskMgrError, TaskMgrResult};

/// Deserialized input for `task-mgr add`.
///
/// Mirrors [`PrdUserStory`] but makes `priority` and `passes` optional so
/// minimal inputs work (priority is auto-computed; absent `passes` means
/// `false` → status `todo`). Anything not supplied here is carried through
/// as the default when the struct is converted into a full `PrdUserStory`.
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
/// `priority` field in the input JSON.
pub fn add(
    db_dir: &Path,
    input_json: &str,
    priority_override: Option<i32>,
    depended_on_by: &[String],
) -> TaskMgrResult<AddResult> {
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

    add_with_conn(&conn, input, priority_override, depended_on_by)
}

/// Testable variant that takes an already-open connection (used by unit tests
/// with in-memory DBs).
pub fn add_with_conn(
    conn: &Connection,
    mut input: AddTaskInput,
    priority_override: Option<i32>,
    depended_on_by: &[String],
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

    // Auto-prefix: when exactly one active PRD prefix exists, prepend it to the
    // task ID and all cross-references. Idempotent — already-prefixed IDs are
    // left unchanged.
    let resolved_ctx = resolve_context(conn)?;

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

    let task_prefix = input.task_prefix().map(String::from);
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
    // the DB — the task is already in the database, and `task-mgr export`
    // can reconcile the JSON later.
    let prd_path = match locate_prd_json(conn, task_prefix.as_deref()) {
        Ok(Some(path)) => match append_task_to_prd_json(
            &path,
            &story,
            effective_depended_on_by,
            task_prefix.as_deref(),
        ) {
            Ok(()) => Some(path),
            Err(e) => {
                ui::emit_err(&format!(
                    "Warning: task {} added to DB but PRD JSON sync failed ({}): {}",
                    story.id,
                    path.display(),
                    e,
                ));
                Some(path)
            }
        },
        Ok(None) => {
            ui::emit(&format!(
                "Note: task {} added to DB; no PRD JSON registered in prd_files — skipping file sync",
                story.id,
            ));
            None
        }
        Err(e) => {
            ui::emit_err(&format!(
                "Warning: task {} added to DB; could not locate PRD JSON: {}",
                story.id, e,
            ));
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

/// How the active prefix was resolved.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    /// Resolved from `TASK_MGR_ACTIVE_PREFIX` environment variable.
    EnvVar,
    /// Resolved because exactly one prefix exists in `prd_metadata`.
    SinglePrefix,
    /// Resolved from an explicit `--from-json` flag (reserved for future use).
    FromJsonFlag,
    /// Could not resolve: env var absent and DB has zero or 2+ prefixes.
    None,
}

impl fmt::Display for ResolutionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolutionSource::EnvVar => write!(f, "env"),
            ResolutionSource::SinglePrefix => write!(f, "single-prefix"),
            ResolutionSource::FromJsonFlag => write!(f, "from-json"),
            ResolutionSource::None => write!(f, "none"),
        }
    }
}

/// Resolved context: which prefix is active, how it was found, and which PRD
/// JSON it maps to. Used by `task-mgr current` and the leading stderr line on
/// write operations.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedContext {
    pub prefix: String,
    pub source: ResolutionSource,
    pub prd_json_path: PathBuf,
}

/// Resolve the active prefix and annotate it with source + target PRD path.
///
/// Returns `Ok(None)` when the DB has zero or 2+ prefixes and `TASK_MGR_ACTIVE_PREFIX`
/// is not set — the caller should treat this as "no active PRD" rather than an
/// error. Propagates `Err` for stale env pins or DB failures.
pub fn resolve_context(conn: &Connection) -> TaskMgrResult<Option<ResolvedContext>> {
    let env_value = std::env::var(crate::loop_engine::claude::ACTIVE_PREFIX_ENV).ok();
    let env_set = env_value.as_deref().is_some_and(|v| !v.is_empty());

    // Delegate all validation logic to resolve_active_prefix (single implementation).
    let prefix_opt = resolve_active_prefix(conn)?;

    let Some(prefix) = prefix_opt else {
        return Ok(None);
    };

    let source = if env_set {
        ResolutionSource::EnvVar
    } else {
        ResolutionSource::SinglePrefix
    };

    let prd_json_path = locate_prd_json(conn, Some(&prefix))?.unwrap_or_default();

    Ok(Some(ResolvedContext {
        prefix,
        source,
        prd_json_path,
    }))
}

/// Returns all non-NULL `task_prefix` values from `prd_metadata`.
///
/// Single canonical home for the `WHERE task_prefix IS NOT NULL` query —
/// callers must not repeat this SQL directly.
fn load_known_prefixes(conn: &Connection) -> TaskMgrResult<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT task_prefix FROM prd_metadata WHERE task_prefix IS NOT NULL")?;
    Ok(stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect())
}

fn prefix_exists_in_metadata(conn: &Connection, prefix: &str) -> TaskMgrResult<bool> {
    let mut stmt = conn.prepare("SELECT 1 FROM prd_metadata WHERE task_prefix = ? LIMIT 1")?;
    let found: Option<i64> = stmt
        .query_row([prefix], |row| row.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(found.is_some())
}

/// Query `prd_metadata` for the active effort prefix.
///
/// Resolution order:
/// 1. If `TASK_MGR_ACTIVE_PREFIX` is set (non-empty), verify the value exists
///    in `prd_metadata.task_prefix`. Return `Ok(Some(value))` on hit; return
///    `Err(invalid_state)` when the env value is NOT registered (stale pin —
///    surfaces typos / cross-PRD leakage immediately). Empty string is treated
///    as unset.
/// 2. Env unset/empty → single-prefix fallback: `Ok(Some(prefix))` when
///    exactly one non-NULL `task_prefix` exists; `Ok(None)` otherwise.
///
/// DB errors propagate via `?` — an unexpected failure must not silently
/// bypass auto-prefixing. `std::env::var` is read exactly once at entry.
fn resolve_active_prefix(conn: &Connection) -> TaskMgrResult<Option<String>> {
    let env_value = std::env::var(crate::loop_engine::claude::ACTIVE_PREFIX_ENV).ok();
    if let Some(env_prefix) = env_value.as_deref().filter(|v| !v.is_empty()) {
        if prefix_exists_in_metadata(conn, env_prefix)? {
            return Ok(Some(env_prefix.to_string()));
        }
        let known = load_known_prefixes(conn)?;
        let known_display = if known.is_empty() {
            "(none registered)".to_string()
        } else {
            known.join(", ")
        };
        return Err(TaskMgrError::invalid_state(
            "add",
            crate::loop_engine::claude::ACTIVE_PREFIX_ENV,
            format!("a prefix registered in prd_metadata (known: {known_display})"),
            format!("{env_prefix} (not found in prd_metadata)"),
        ));
    }
    let prefixes = load_known_prefixes(conn)?;
    if prefixes.len() == 1 {
        Ok(Some(prefixes.into_iter().next().unwrap()))
    } else {
        Ok(None)
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
/// * Foreign prefix registered in `prd_metadata` → refuse with the PRD's
///   actual path AND both fix commands.
/// * Foreign prefix not registered anywhere → refuse with a hint pointing at
///   `task-mgr list --prefix <p>`.
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
        let foreign_path = locate_prd_json(conn, Some(target_prefix))?;
        let active_prefix = &ctx.prefix;
        return Err(match foreign_path {
            Some(path) => TaskMgrError::invalid_state(
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
            ),
            None => TaskMgrError::invalid_state(
                "add",
                "depended-on-by",
                format!("a task id belonging to the active PRD (prefix '{active_prefix}')"),
                format!(
                    "Refusing: target '{target_id}' carries prefix '{target_prefix}', which is \
                     not registered in any known PRD. Run `task-mgr list --prefix {target_prefix}` \
                     to find the right id."
                ),
            ),
        });
    }
    Ok(())
}

/// Look up the PRD JSON file path for the currently-active PRD.
///
/// When `task_prefix` is provided, finds the PRD JSON via `prd_metadata`.
/// Falls back to the first registered `task_list` file when prefix is `None`
/// or the prefix-scoped query finds nothing.
///
/// Returns `Ok(None)` when no `task_list` file is registered (valid state:
/// e.g. the DB was populated programmatically without a source JSON).
fn locate_prd_json(conn: &Connection, task_prefix: Option<&str>) -> TaskMgrResult<Option<PathBuf>> {
    if let Some(prefix) = task_prefix {
        let result: Option<String> = conn
            .query_row(
                "SELECT pf.file_path FROM prd_files pf \
                 JOIN prd_metadata pm ON pf.prd_id = pm.id \
                 WHERE pf.file_type = 'task_list' AND pm.task_prefix = ? \
                 LIMIT 1",
                [prefix],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some(path) = result {
            return Ok(Some(PathBuf::from(path)));
        }
    }
    // Fallback: first registered task_list regardless of prefix.
    let mut stmt =
        conn.prepare("SELECT file_path FROM prd_files WHERE file_type = 'task_list' LIMIT 1")?;
    let path: Option<String> =
        stmt.query_row([], |row| row.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
    Ok(path.map(PathBuf::from))
}

/// Append a serialized task to the PRD JSON's `userStories` array, atomically.
///
/// Uses a temp-file + rename so a crash mid-write does not corrupt the PRD.
///
/// When `depended_on_by` is non-empty, each existing userStories entry matching
/// one of those ids gets the new task's id pushed into its `dependsOn` array
/// (creating the array if missing). Targets that aren't found in the JSON are
/// skipped with a warning — the DB is authoritative.
/// Strip the active prefix from every string element of the `key` array on a
/// serialized userStory object, rewriting the DB-prefixed relationship ids into
/// the unprefixed JSON convention. No-op when the key is absent or not an array.
fn strip_prefix_in_id_array(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    prefix: Option<&str>,
) {
    let Some(arr) = obj.get_mut(key).and_then(|v| v.as_array_mut()) else {
        return;
    };
    for elem in arr.iter_mut() {
        if let Some(id) = elem.as_str() {
            *elem = Value::String(strip_task_prefix(id, prefix).to_string());
        }
    }
}

fn append_task_to_prd_json(
    prd_path: &Path,
    story: &PrdUserStory,
    depended_on_by: &[String],
    prefix: Option<&str>,
) -> TaskMgrResult<()> {
    // The DB stores prefixed ids (`e474b6f2-CODE-REVIEW-2`) but PRD task-list
    // JSON stores them unprefixed (`CODE-REVIEW-2`); the importer re-applies the
    // prefix idempotently. Mirror `prd_reconcile::update_prd_task_passes` and
    // write/match ids in the JSON's unprefixed convention so the new entry stays
    // consistent with its siblings and the reverse-link sync actually lands.
    let base_story_id = strip_task_prefix(&story.id, prefix);
    let original = fs::read_to_string(prd_path).map_err(|e| {
        TaskMgrError::invalid_state(
            "add",
            "prd file",
            "readable",
            format!("{}: {}", prd_path.display(), e),
        )
    })?;

    let mut root: Value = serde_json::from_str(&original).map_err(|e| {
        TaskMgrError::invalid_state(
            "add",
            "prd json",
            "valid JSON object",
            format!("{}: {}", prd_path.display(), e),
        )
    })?;

    let root_obj = root.as_object_mut().ok_or_else(|| {
        TaskMgrError::invalid_state("add", "prd json", "JSON object at root", "not an object")
    })?;

    // Reject duplicate IDs already present in the file (defence-in-depth —
    // DB check would have caught this too, unless someone hand-edited the
    // JSON out-of-band).
    if let Some(user_stories) = root_obj.get("userStories").and_then(|v| v.as_array()) {
        let dup = user_stories.iter().any(|t| {
            t.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == story.id || id == base_story_id)
        });
        if dup {
            return Err(TaskMgrError::invalid_state(
                "add",
                "task id",
                "not already present in PRD JSON",
                format!("{} already in {}", story.id, prd_path.display()),
            ));
        }
    }

    // Serialize the new entry, then rewrite its id-bearing fields into the
    // unprefixed JSON convention (the struct carries the prefixed DB forms).
    let mut task_value = serde_json::to_value(story)?;
    if let Some(obj) = task_value.as_object_mut() {
        obj.insert("id".to_string(), Value::String(base_story_id.to_string()));
        for key in ["dependsOn", "synergyWith", "batchWith", "conflictsWith"] {
            strip_prefix_in_id_array(obj, key, prefix);
        }
    }

    let arr = root_obj
        .entry("userStories")
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = arr.as_array_mut().ok_or_else(|| {
        TaskMgrError::invalid_state(
            "add",
            "userStories",
            "JSON array",
            "present but not an array",
        )
    })?;

    // Reverse-link updates: for each requested existing task id, find its
    // entry and push story.id into its dependsOn array (creating if missing).
    // Missing targets log a warning but don't fail — DB is the source of truth.
    for existing_id in depended_on_by {
        // The JSON may carry either convention (originally-authored entries are
        // unprefixed; entries spawned by an older `add` may be prefixed), so
        // match on both the prefixed target and its unprefixed base.
        let base_target = strip_task_prefix(existing_id, prefix);
        let mut matched = false;
        for entry in arr.iter_mut() {
            let Some(obj) = entry.as_object_mut() else {
                continue;
            };
            let is_match = obj
                .get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == existing_id || id == base_target);
            if !is_match {
                continue;
            }
            matched = true;
            let deps_entry = obj
                .entry("dependsOn".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let deps_arr = deps_entry.as_array_mut().ok_or_else(|| {
                TaskMgrError::invalid_state(
                    "add",
                    "dependsOn",
                    "JSON array",
                    format!("{} dependsOn present but not an array", existing_id),
                )
            })?;
            // Push the unprefixed new id; dedup against both forms in case the
            // array already mixes conventions.
            let already = deps_arr.iter().any(|v| {
                v.as_str()
                    .is_some_and(|s| s == story.id || s == base_story_id)
            });
            if !already {
                deps_arr.push(Value::String(base_story_id.to_string()));
            }
            break;
        }
        if !matched {
            ui::emit_err(&format!(
                "Warning: --depended-on-by target {} not found in PRD JSON {}; DB updated but JSON dependsOn not synced for that target",
                existing_id,
                prd_path.display(),
            ));
        }
    }

    arr.push(task_value);

    let pretty = serde_json::to_string_pretty(&root)?;
    // Preserve trailing newline if original had one.
    let output = if original.ends_with('\n') {
        format!("{}\n", pretty)
    } else {
        pretty
    };

    atomic_write(prd_path, &output)?;
    Ok(())
}

/// Write `content` to `target` atomically (tmp file + rename).
fn atomic_write(target: &Path, content: &str) -> TaskMgrResult<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let tmp_name = match target.file_name() {
        Some(n) => format!(".{}.task-mgr-add.tmp", n.to_string_lossy()),
        None => ".task-mgr-add.tmp".to_string(),
    };
    let tmp_path = parent.join(tmp_name);
    fs::write(&tmp_path, content).map_err(|e| {
        TaskMgrError::invalid_state(
            "add",
            "prd file write",
            "successful tmp write",
            format!("{}: {}", tmp_path.display(), e),
        )
    })?;
    fs::rename(&tmp_path, target).map_err(|e| {
        // Best-effort cleanup.
        let _ = fs::remove_file(&tmp_path);
        TaskMgrError::invalid_state(
            "add",
            "prd file rename",
            "successful rename",
            format!("{} -> {}: {}", tmp_path.display(), target.display(), e),
        )
    })?;
    Ok(())
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
        }
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
        let err = add_with_conn(&conn, input, None, &[]).unwrap_err();
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
        let res = add_with_conn(&conn, input, None, &[]).unwrap();
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
        add_with_conn(&conn, input, None, &[]).unwrap();

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

    #[test]
    fn test_locate_prd_json_returns_none_when_no_file_registered() {
        let conn = memory_db();
        let path = locate_prd_json(&conn, None).unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn test_append_task_to_prd_json_adds_to_userstories() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial = r#"{
  "project": "demo",
  "userStories": [
    {"id": "SEED-001", "title": "seed", "priority": 50, "passes": false}
  ]
}
"#;
        {
            let mut f = tmp.reopen().unwrap();
            f.write_all(initial.as_bytes()).unwrap();
        }

        let story = PrdUserStory {
            id: "NEW-001".to_string(),
            title: "new".to_string(),
            description: None,
            priority: 5,
            passes: false,
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
        };

        append_task_to_prd_json(tmp.path(), &story, &[], None).unwrap();

        let after = fs::read_to_string(tmp.path()).unwrap();
        let v: Value = serde_json::from_str(&after).unwrap();
        let arr = v
            .get("userStories")
            .and_then(|v| v.as_array())
            .expect("userStories array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1].get("id").and_then(|v| v.as_str()), Some("NEW-001"));
        assert_eq!(arr[1].get("priority").and_then(|v| v.as_i64()), Some(5));
        assert!(after.ends_with('\n'), "trailing newline preserved");
    }

    #[test]
    fn test_append_rejects_duplicate_id_in_prd_file() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial =
            r#"{"userStories":[{"id":"DUP-001","title":"x","priority":50,"passes":false}]}"#;
        {
            let mut f = tmp.reopen().unwrap();
            f.write_all(initial.as_bytes()).unwrap();
        }
        let story = PrdUserStory {
            id: "DUP-001".to_string(),
            title: "again".to_string(),
            description: None,
            priority: 5,
            passes: false,
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
        };
        let err = append_task_to_prd_json(tmp.path(), &story, &[], None).unwrap_err();
        assert!(format!("{err}").contains("DUP-001"));
    }

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
        add_with_conn(&conn, input, None, &["MILESTONE-1".to_string()]).unwrap();

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
        let err = add_with_conn(&conn, input, None, &["NONEXISTENT-ID".to_string()]).unwrap_err();
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

    // --- FEAT-002: resolve_active_prefix env-var awareness ---

    fn seed_prefix(conn: &Connection, id: i64, project: &str, task_prefix: &str) {
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (?, ?, ?)",
            rusqlite::params![id, project, task_prefix],
        )
        .unwrap();
    }

    #[test]
    fn test_resolve_prefix_env_unset_single_prd_returns_some() {
        // Regression: existing single-PRD fallback behavior preserved.
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        let resolved = resolve_active_prefix(&conn).unwrap();
        assert_eq!(resolved, Some("A-".to_string()));
    }

    #[test]
    fn test_resolve_prefix_env_unset_multi_prd_returns_none() {
        // Regression: ambiguous (multiple) prefixes return None — caller
        // skips auto-prefixing and relies on caller-supplied IDs.
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        seed_prefix(&conn, 2, "beta", "B-");
        let resolved = resolve_active_prefix(&conn).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn test_resolve_prefix_env_set_matching_multi_prd_returns_pinned() {
        // Pinning resolves the multi-PRD ambiguity.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "A-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        seed_prefix(&conn, 2, "beta", "B-");
        let resolved = resolve_active_prefix(&conn).unwrap();
        assert_eq!(resolved, Some("A-".to_string()));
    }

    #[test]
    fn test_resolve_prefix_env_set_stale_errors_with_known_prefixes() {
        // Stale pin (set but not registered) must hard-error and the message
        // must name BOTH the offending value and the registered prefixes
        // so the operator can spot the typo.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "stale-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        let err = resolve_active_prefix(&conn).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("stale-"), "error must name stale value: {msg}");
        assert!(msg.contains("A-"), "error must list known prefixes: {msg}");
    }

    #[test]
    fn test_resolve_prefix_env_empty_string_treated_as_unset() {
        // Empty env var must NOT trigger the stale-prefix error path —
        // shells often export blank values when a variable was deliberately
        // cleared. Falls through to fallback (multi-PRD → None).
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        seed_prefix(&conn, 2, "beta", "B-");
        let resolved = resolve_active_prefix(&conn).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn test_resolve_prefix_env_set_with_empty_metadata_errors() {
        // No PRDs registered + env var set → still stale (no prefix is valid).
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "X-");
        let conn = memory_db();
        let err = resolve_active_prefix(&conn).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("X-"), "error must name stale value: {msg}");
        assert!(
            msg.contains("none registered") || msg.contains("not found"),
            "error must indicate empty/missing prefix set: {msg}"
        );
    }

    #[test]
    fn test_resolve_prefix_naive_passthrough_would_fail_stale_check() {
        // Known-bad guard: a naive `Ok(Some(env_value))` impl would pass
        // through unverified. This test pins env to a value that is NOT in
        // prd_metadata and asserts we get Err — defeating the safety check
        // by skipping verification must fail this assertion.
        let _iso = isolate_env();
        let _set = EnvVarGuard::set(ACTIVE_PREFIX_ENV, "B-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        assert!(
            resolve_active_prefix(&conn).is_err(),
            "must reject pinned prefix not present in prd_metadata"
        );
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
        let res = add_with_conn(&conn, input, None, &[]).unwrap();

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
    /// machinery — locate_prd_json is the only DB-touching side effect.
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
        let err =
            add_with_conn(&conn, input, None, &["beta-MILESTONE-FINAL".to_string()]).unwrap_err();
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
        );
        assert!(
            res.is_ok(),
            "same-PRD add must still succeed: {:?}",
            res.err()
        );
        let r = res.unwrap();
        assert_eq!(r.task_id, "5ba153a7-NEW-FIX-001");
    }
}
