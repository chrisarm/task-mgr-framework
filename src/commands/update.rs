//! `task-mgr update` overlay validate + load-merge-write (CONTRACT-001/002/003).
//!
//! - FEAT-003: Value-only whitelist validator (`validate_update_overlay`).
//! - FEAT-004: `update_with_conn` — partial `UPDATE tasks`, scoped `dependsOn`
//!   delete, JSON-only refuse vs mixed pin 11, `patch_user_story` persist.
//! - FEAT-005: clap + `update()` pin/write-policy wrapper (`default_prd_roots`
//!   → `resolve_context_with_roots`; shared `preflight_from_json_path` /
//!   `refuse_unpinned_write`). Does **not** `use commands::add`.
//!
//! Never call `import::update_task` or `delete_task_relationships`.
//!
//! Full contracts: `## CONTRACT-001` / `002` / `003` in `tasks/progress-a8855e28.txt`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::commands::context::{
    ResolvedContext, choose_cli_write_path, cli_write_path, default_prd_roots,
    preflight_from_json_path, refuse_unpinned_write, resolve_context_with_roots,
    sole_task_list_path,
};
use crate::commands::init::import::{delete_task_files, insert_relationship, insert_task_file};
use crate::commands::init::prefix_id;
use crate::commands::prd_json::patch_user_story;
use crate::output::ui;
use crate::{TaskMgrError, TaskMgrResult};

/// JSON camelCase whitelist + lookup `id` (CONTRACT-002).
const OVERLAY_WHITELIST: &[&str] = &[
    "title",
    "description",
    "notes",
    "acceptanceCriteria",
    "touchesFiles",
    "dependsOn",
    "estimatedEffort",
    "difficulty",
    "model",
    "escalationNote",
    "requiredTests",
    "maxRetries",
    "requiresHuman",
    "humanReviewTimeout",
    "claimsSharedInfra",
    "reviewScope",
    "severity",
    "sourceReview",
    "humanReviewOutcome",
];

/// Present overlay value that may be JSON `null` (clear) or a typed value.
///
/// Distinguishes key-absent (`Option::None` on the field) from key-present-null
/// (`Some(Nullable::Null)`) for FEAT-004 merge.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Nullable<T> {
    Null,
    Present(T),
}

/// Validated overlay ready for FEAT-004 merge. Keys are JSON camelCase.
/// Presence means "apply"; absent means "leave unchanged".
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ValidatedUpdateOverlay {
    /// Lookup-only; never SET on DB / never copied onto JSON story id.
    pub id: String,
    pub title: Option<String>,
    pub description: Option<Nullable<String>>,
    pub notes: Option<Nullable<String>>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub touches_files: Option<Vec<String>>,
    pub depends_on: Option<Vec<String>>,
    /// From either `estimatedEffort` or `difficulty` (not both — rejected).
    pub difficulty: Option<Nullable<String>>,
    pub model: Option<Nullable<String>>,
    pub escalation_note: Option<Nullable<String>>,
    pub required_tests: Option<Vec<String>>,
    pub max_retries: Option<i32>,
    pub requires_human: Option<bool>,
    pub human_review_timeout: Option<Nullable<u32>>,
    pub claims_shared_infra: Option<Nullable<bool>>,
    pub review_scope: Option<Nullable<Value>>,
    pub severity: Option<Nullable<String>>,
    pub source_review: Option<Nullable<String>>,
    /// Object replaces; `Null` removes the JSON key (CONTRACT-003).
    pub human_review_outcome: Option<Nullable<Value>>,
}

/// Validate an update overlay `Value` (CONTRACT-002).
///
/// Order is load-bearing: object shape → top-level `status`/`passes` →
/// unknown keys (name all) → effort alias ambiguity → type/null table.
/// Never deserializes through `PrdUserStory`.
pub(crate) fn validate_update_overlay(overlay: &Value) -> TaskMgrResult<ValidatedUpdateOverlay> {
    let obj = overlay.as_object().ok_or_else(|| {
        TaskMgrError::invalid_state("update", "overlay", "JSON object", value_kind(overlay))
    })?;

    // Step 2: lifecycle keys (any value) before unknown-key / type tables.
    if obj.contains_key("status") || obj.contains_key("passes") {
        return Err(lifecycle_reject(obj));
    }

    // Step 3: unknown keys — name ALL present unknowns.
    let mut unknown: Vec<&str> = obj
        .keys()
        .filter(|k| k.as_str() != "id" && !OVERLAY_WHITELIST.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(TaskMgrError::invalid_state(
            "update",
            "overlay",
            "whitelist keys only (see `task-mgr update` docs)",
            format!("unknown keys: {}", unknown.join(", ")),
        ));
    }

    // Step 5 (presence check, any values): both effort aliases → ambiguous.
    // Checked before type/null so wrong-typed dual keys still get ambiguity.
    let has_effort = obj.contains_key("estimatedEffort");
    let has_difficulty = obj.contains_key("difficulty");
    if has_effort && has_difficulty {
        return Err(TaskMgrError::invalid_state(
            "update",
            "overlay",
            "exactly one of estimatedEffort or difficulty",
            "both estimatedEffort and difficulty present (ambiguous)",
        ));
    }

    // Step 4: type / null table (and required lookup id).
    let id = require_nonempty_string(obj, "id")?;

    let title = optional_nonempty_string(obj, "title")?;
    let description = optional_nullable_string(obj, "description")?;
    let notes = optional_nullable_string(obj, "notes")?;
    let model = optional_nullable_string(obj, "model")?;
    let escalation_note = optional_nullable_string(obj, "escalationNote")?;
    let severity = optional_nullable_string(obj, "severity")?;
    let source_review = optional_nullable_string(obj, "sourceReview")?;

    let difficulty = if has_effort {
        optional_nullable_string(obj, "estimatedEffort")?
    } else if has_difficulty {
        optional_nullable_string(obj, "difficulty")?
    } else {
        None
    };

    let acceptance_criteria = optional_string_array(obj, "acceptanceCriteria")?;
    let touches_files = optional_string_array(obj, "touchesFiles")?;
    let depends_on = optional_string_array(obj, "dependsOn")?;
    let required_tests = optional_string_array(obj, "requiredTests")?;

    let requires_human = optional_required_bool(obj, "requiresHuman")?;
    let max_retries = optional_required_i32(obj, "maxRetries")?;
    let human_review_timeout = optional_nullable_u32(obj, "humanReviewTimeout")?;
    let claims_shared_infra = optional_nullable_bool(obj, "claimsSharedInfra")?;
    let review_scope = optional_nullable_value(obj, "reviewScope")?;
    let human_review_outcome = optional_outcome(obj, "humanReviewOutcome")?;

    Ok(ValidatedUpdateOverlay {
        id,
        title,
        description,
        notes,
        acceptance_criteria,
        touches_files,
        depends_on,
        difficulty,
        model,
        escalation_note,
        required_tests,
        max_retries,
        requires_human,
        human_review_timeout,
        claims_shared_infra,
        review_scope,
        severity,
        source_review,
        human_review_outcome,
    })
}

fn lifecycle_reject(obj: &Map<String, Value>) -> TaskMgrError {
    let mut keys = Vec::new();
    if obj.contains_key("status") {
        keys.push("status");
    }
    if obj.contains_key("passes") {
        keys.push("passes");
    }
    TaskMgrError::invalid_state(
        "update",
        "overlay",
        "whitelist fields only — status/passes are lifecycle (use `task-mgr complete` / `fail` / `skip` / `<task-status>`)",
        format!("top-level {} present", keys.join(" and ")),
    )
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_err(key: &str, expected: &str, actual: &Value) -> TaskMgrError {
    TaskMgrError::invalid_state("update", key, expected, value_kind(actual))
}

fn require_nonempty_string(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<String> {
    match obj.get(key) {
        None => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "non-empty string (required lookup id)",
            "missing",
        )),
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "non-empty string",
            "empty string",
        )),
        Some(other) => Err(type_err(key, "non-empty string", other)),
    }
}

fn optional_nonempty_string(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<String>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "non-empty string",
            "empty string",
        )),
        Some(other) => Err(type_err(key, "non-empty string", other)),
    }
}

fn optional_nullable_string(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<String>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(Value::String(s)) => Ok(Some(Nullable::Present(s.clone()))),
        Some(other) => Err(type_err(key, "string or null", other)),
    }
}

fn optional_string_array(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Vec<String>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "array of strings ([] clears; null is illegal)",
            "null",
        )),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, elem) in arr.iter().enumerate() {
                match elem.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(TaskMgrError::invalid_state(
                            "update",
                            key,
                            "array of strings",
                            format!("element[{i}] is {}", value_kind(elem)),
                        ));
                    }
                }
            }
            Ok(Some(out))
        }
        Some(other) => Err(type_err(key, "array of strings", other)),
    }
}

/// `requiresHuman`: JSON bool only — null / non-bool reject (stricter than serde Option).
fn optional_required_bool(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<bool>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::Null) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "bool (null is illegal)",
            "null",
        )),
        Some(other) => Err(type_err(key, "bool", other)),
    }
}

/// `maxRetries`: JSON integer only — null rejects (column NOT NULL; do not default to 3).
fn optional_required_i32(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<i32>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "integer (null is illegal; column NOT NULL)",
            "null",
        )),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i32::try_from(i).map(Some).map_err(|_| {
                    TaskMgrError::invalid_state(
                        "update",
                        key,
                        "integer fitting i32",
                        format!("{i}"),
                    )
                })
            } else {
                Err(TaskMgrError::invalid_state(
                    "update",
                    key,
                    "integer",
                    "non-integer number",
                ))
            }
        }
        Some(other) => Err(type_err(key, "integer", other)),
    }
}

fn optional_nullable_u32(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<u32>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                u32::try_from(u)
                    .map(|v| Some(Nullable::Present(v)))
                    .map_err(|_| {
                        TaskMgrError::invalid_state(
                            "update",
                            key,
                            "unsigned integer fitting u32 or null",
                            format!("{u}"),
                        )
                    })
            } else if n.as_i64().is_some_and(|i| i < 0) {
                Err(TaskMgrError::invalid_state(
                    "update",
                    key,
                    "unsigned integer or null",
                    "negative integer",
                ))
            } else {
                Err(TaskMgrError::invalid_state(
                    "update",
                    key,
                    "unsigned integer or null",
                    "non-integer number",
                ))
            }
        }
        Some(other) => Err(type_err(key, "unsigned integer or null", other)),
    }
}

fn optional_nullable_bool(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<bool>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(Value::Bool(b)) => Ok(Some(Nullable::Present(*b))),
        Some(other) => Err(type_err(key, "bool or null", other)),
    }
}

/// `reviewScope`: any JSON value or null clears — do not `as_str()`.
fn optional_nullable_value(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<Value>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(v) => Ok(Some(Nullable::Present(v.clone()))),
    }
}

/// `humanReviewOutcome`: object or null (null removes JSON key). Nested
/// `passes` inside the object is data, not a top-level lifecycle key.
fn optional_outcome(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<Nullable<Value>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(v) if v.is_object() => Ok(Some(Nullable::Present(v.clone()))),
        Some(other) => Err(type_err(key, "object or null", other)),
    }
}

// ---------------------------------------------------------------------------
// FEAT-004: load-merge-write (`update_with_conn`)
// ---------------------------------------------------------------------------

/// Result of a successful `task-mgr update` (CONTRACT-001).
///
/// Distinct from [`crate::commands::run::UpdateResult`] (run-session).
#[derive(Debug, Clone, Serialize)]
pub struct UpdateResult {
    pub task_id: String,
    /// Overlay keys that caused a DB and/or JSON write (JSON camelCase).
    pub fields_updated: Vec<String>,
    /// `Some(path)` if JSON was patched; `None` if mixed + skipped (pin 11).
    pub prd_path: Option<PathBuf>,
}

/// Entry point for `task-mgr update`.
///
/// Pin order matches add (CONTRACT-001 / FEAT-005):
/// 1. `--from-json` missing/directory via [`preflight_from_json_path`] **before**
///    overlay parse
/// 2. Parse overlay JSON (+ validate)
/// 3. `LockGuard` + open conn
/// 4. [`default_prd_roots`] → [`resolve_context_with_roots`] (not bare
///    [`crate::commands::context::resolve_context`])
/// 5. [`refuse_unpinned_write`] before the write txn
///
/// Does **not** import `commands::add`. `--from-json` never inserts
/// `prd_files` / `prd_metadata`.
pub fn update(
    db_dir: &Path,
    input_json: &str,
    from_json: Option<&Path>,
) -> TaskMgrResult<UpdateResult> {
    if let Some(path) = from_json {
        preflight_from_json_path(path, "update")?;
    }

    let input: Value = serde_json::from_str(input_json).map_err(|e| {
        TaskMgrError::invalid_state(
            "update",
            "input JSON",
            "valid overlay JSON object (fields: id, whitelist keys)",
            format!("parse error: {e}"),
        )
    })?;
    // Validate before lock when possible (CONTRACT-002 call order).
    validate_update_overlay(&input)?;

    let _lock = crate::db::LockGuard::acquire(db_dir)?;
    let conn = crate::db::open_connection(db_dir)?;
    let (source_root, worktree_root) = default_prd_roots(db_dir);

    update_with_conn_in(
        &conn,
        &input,
        from_json,
        Some(&source_root),
        Some(&worktree_root),
    )
}

/// Testable variant with an already-open connection (in-memory / fixture DBs).
///
/// Resolves pin + write path the same way as [`update`], then merges via
/// [`update_with_conn`]. Pass `None` roots only for pure in-memory tests that
/// do not exercise remap.
pub fn update_with_roots(
    conn: &Connection,
    input: &Value,
    from_json: Option<&Path>,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<UpdateResult> {
    update_with_conn_in(conn, input, from_json, source_root, worktree_root)
}

fn update_with_conn_in(
    conn: &Connection,
    input: &Value,
    from_json: Option<&Path>,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<UpdateResult> {
    let resolved_ctx =
        resolve_context_with_roots(conn, from_json, "update", source_root, worktree_root)?;
    refuse_unpinned_write(conn, &resolved_ctx, "update")?;

    // Emit resolved-context line as the FIRST stderr output (same as add).
    if let Some(ref ctx) = resolved_ctx {
        let target = if ctx.prd_json_path.as_os_str().is_empty() {
            "(none)".to_string()
        } else {
            ctx.prd_json_path.display().to_string()
        };
        ui::emit(&format!(
            "→ active prefix={}  source={}  target={}",
            ctx.prefix, ctx.source, target,
        ));
    }

    let (write_path, prefix) =
        resolve_update_write_target(conn, &resolved_ctx, source_root, worktree_root)?;

    update_with_conn(conn, input, write_path.as_deref(), &prefix)
}

/// Choose JSON write path + prefix after pin resolution.
///
/// - `Some(ctx)` → `ctx.prd_json_path` only (`--from-json` = canonical PATH;
///   default = remap-then-`is_file()` already stored). Empty OsStr → no path.
/// - `None` → [`sole_task_list_path`] then [`choose_cli_write_path`] when roots
///   known, else [`cli_write_path`]. Empty prefix (zero-prefix / `--no-prefix`).
fn resolve_update_write_target(
    conn: &Connection,
    resolved_ctx: &Option<ResolvedContext>,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<(Option<PathBuf>, String)> {
    match resolved_ctx {
        Some(ctx) => {
            let path = if ctx.prd_json_path.as_os_str().is_empty() {
                None
            } else {
                Some(ctx.prd_json_path.clone())
            };
            Ok((path, ctx.prefix.clone()))
        }
        None => {
            let path = match sole_task_list_path(conn)? {
                Some(registered) => {
                    let chosen = match (source_root, worktree_root) {
                        (Some(src), Some(wt)) => choose_cli_write_path(&registered, src, wt),
                        _ => cli_write_path(&registered),
                    };
                    if chosen.as_os_str().is_empty() {
                        None
                    } else {
                        Some(chosen)
                    }
                }
                None => None,
            };
            Ok((path, String::new()))
        }
    }
}

/// Format text output for a successful update.
pub fn format_text(result: &UpdateResult) -> String {
    let fields = if result.fields_updated.is_empty() {
        "(none)".to_string()
    } else {
        result.fields_updated.join(", ")
    };
    let mut out = format!("Updated task {} (fields: {})", result.task_id, fields);
    if let Some(p) = &result.prd_path {
        out.push_str(&format!("\nSynced into PRD JSON: {}", p.display()));
    } else {
        out.push_str("\nPRD JSON: no file synced (DB-only or skipped)");
    }
    out
}

/// Load-merge-write given an already-resolved write path and prefix.
///
/// Empty `prefix` skips `prefix_id` (learning #5597). `write_path` of `None`
/// or empty OsStr means no JSON target.
///
/// Never calls `import::update_task` or `delete_task_relationships`.
pub fn update_with_conn(
    conn: &Connection,
    input: &Value,
    write_path: Option<&Path>,
    prefix: &str,
) -> TaskMgrResult<UpdateResult> {
    let overlay = validate_update_overlay(input)?;
    if !has_updatable_fields(&overlay) {
        return Err(TaskMgrError::invalid_state(
            "update",
            "overlay",
            "at least one updatable whitelist field",
            "no updatable fields (id-only)",
        ));
    }

    let task_id = if prefix.is_empty() {
        overlay.id.clone()
    } else {
        prefix_id(prefix, &overlay.id)
    };

    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE id = ?",
        [&task_id],
        |row| row.get(0),
    )?;
    if exists == 0 {
        return Err(TaskMgrError::invalid_state(
            "update",
            "task id",
            "existing task id",
            format!("{task_id} not found in database"),
        ));
    }

    let fields_updated = overlay_keys_updated(input);
    let json_only = is_json_only(&overlay);
    let patch_prefix = if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    };

    if json_only {
        // CONTRACT-001 / 003: JSON-only cannot Ok-skip; no UPDATE tasks.
        let Some(path) = write_path.filter(|p| !p.as_os_str().is_empty()) else {
            return Err(json_only_refuse(
                "no writable task-list path (empty / missing write path)",
            ));
        };
        patch_user_story(path, &task_id, input, patch_prefix, "update").map_err(|e| {
            json_only_refuse(&format!("PRD JSON patch failed ({}): {e}", path.display()))
        })?;
        return Ok(UpdateResult {
            task_id,
            fields_updated,
            prd_path: Some(path.to_path_buf()),
        });
    }

    // Mixed (or DB-only): commit DB first, then best-effort JSON (pin 11).
    apply_db_updates(conn, &task_id, &overlay, prefix)?;

    let prd_path = match write_path.filter(|p| !p.as_os_str().is_empty()) {
        None => {
            ui::emit(&format_mixed_json_skip(&task_id));
            None
        }
        Some(path) => match patch_user_story(path, &task_id, input, patch_prefix, "update") {
            Ok(()) => Some(path.to_path_buf()),
            Err(e) => {
                ui::emit_err(&format_mixed_json_failure(&task_id, path, &e));
                None
            }
        },
    };

    Ok(UpdateResult {
        task_id,
        fields_updated,
        prd_path,
    })
}

fn json_only_refuse(actual: &str) -> TaskMgrError {
    TaskMgrError::invalid_state(
        "update",
        "prd json write",
        "writable task-list path — run `task-mgr current`, then retry with --from-json <path>",
        actual,
    )
}

fn json_sync_recovery_hint() -> &'static str {
    "Run `task-mgr current` to inspect the write target, then retry with --from-json <path>"
}

fn stale_json_note() -> &'static str {
    "Note: a later `loop init --append --update-existing` may SET from stale JSON"
}

fn format_mixed_json_failure(task_id: &str, path: &Path, err: &TaskMgrError) -> String {
    format!(
        "Warning: task {task_id} updated in DB but PRD JSON sync failed ({}): {err}. {}. {}.",
        path.display(),
        json_sync_recovery_hint(),
        stale_json_note(),
    )
}

fn format_mixed_json_skip(task_id: &str) -> String {
    format!(
        "Note: task {task_id} updated in DB; no PRD JSON write path — skipping file sync. {}. {}.",
        json_sync_recovery_hint(),
        stale_json_note(),
    )
}

fn has_updatable_fields(o: &ValidatedUpdateOverlay) -> bool {
    o.title.is_some()
        || o.description.is_some()
        || o.notes.is_some()
        || o.acceptance_criteria.is_some()
        || o.touches_files.is_some()
        || o.depends_on.is_some()
        || o.difficulty.is_some()
        || o.model.is_some()
        || o.escalation_note.is_some()
        || o.required_tests.is_some()
        || o.max_retries.is_some()
        || o.requires_human.is_some()
        || o.human_review_timeout.is_some()
        || o.claims_shared_infra.is_some()
        || o.review_scope.is_some()
        || o.severity.is_some()
        || o.source_review.is_some()
        || o.human_review_outcome.is_some()
}

/// True when the only updatable key is `humanReviewOutcome` (object or null).
fn is_json_only(o: &ValidatedUpdateOverlay) -> bool {
    o.human_review_outcome.is_some()
        && o.title.is_none()
        && o.description.is_none()
        && o.notes.is_none()
        && o.acceptance_criteria.is_none()
        && o.touches_files.is_none()
        && o.depends_on.is_none()
        && o.difficulty.is_none()
        && o.model.is_none()
        && o.escalation_note.is_none()
        && o.required_tests.is_none()
        && o.max_retries.is_none()
        && o.requires_human.is_none()
        && o.human_review_timeout.is_none()
        && o.claims_shared_infra.is_none()
        && o.review_scope.is_none()
        && o.severity.is_none()
        && o.source_review.is_none()
}

/// Overlay keys (camelCase) that will drive a DB and/or JSON write.
fn overlay_keys_updated(input: &Value) -> Vec<String> {
    let Some(obj) = input.as_object() else {
        return Vec::new();
    };
    let mut keys: Vec<String> = obj
        .keys()
        .filter(|k| k.as_str() != "id" && OVERLAY_WHITELIST.contains(&k.as_str()))
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn nullable_text(n: &Nullable<String>) -> SqlValue {
    match n {
        Nullable::Null => SqlValue::Null,
        Nullable::Present(s) => SqlValue::Text(s.clone()),
    }
}

fn nullable_bool_i64(n: &Nullable<bool>) -> SqlValue {
    match n {
        Nullable::Null => SqlValue::Null,
        Nullable::Present(b) => SqlValue::Integer(i64::from(*b)),
    }
}

fn nullable_u32(n: &Nullable<u32>) -> SqlValue {
    match n {
        Nullable::Null => SqlValue::Null,
        Nullable::Present(u) => SqlValue::Integer(i64::from(*u)),
    }
}

/// Partial `UPDATE tasks` + scoped dependsOn / touchesFiles replace.
///
/// Issues `UPDATE tasks` only when a DB column is present **or** a table
/// mutation (`dependsOn` / `touchesFiles`) occurs — always includes
/// `updated_at`. Never SETs `status`, `archived_at`, `priority`, or `id`.
fn apply_db_updates(
    conn: &Connection,
    task_id: &str,
    overlay: &ValidatedUpdateOverlay,
    prefix: &str,
) -> TaskMgrResult<()> {
    let tx = conn.unchecked_transaction()?;

    let mut set_clauses: Vec<&'static str> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();

    if let Some(ref title) = overlay.title {
        set_clauses.push("title = ?");
        params.push(SqlValue::Text(title.clone()));
    }
    if let Some(ref d) = overlay.description {
        set_clauses.push("description = ?");
        params.push(nullable_text(d));
    }
    if let Some(ref n) = overlay.notes {
        set_clauses.push("notes = ?");
        params.push(nullable_text(n));
    }
    if let Some(ref ac) = overlay.acceptance_criteria {
        set_clauses.push("acceptance_criteria = ?");
        let encoded = serde_json::to_string(ac).map_err(|e| {
            TaskMgrError::invalid_state(
                "update",
                "acceptanceCriteria",
                "JSON-encodable array",
                e.to_string(),
            )
        })?;
        params.push(SqlValue::Text(encoded));
    }
    if let Some(ref diff) = overlay.difficulty {
        set_clauses.push("difficulty = ?");
        params.push(nullable_text(diff));
    }
    if let Some(ref m) = overlay.model {
        set_clauses.push("model = ?");
        params.push(nullable_text(m));
    }
    if let Some(ref e) = overlay.escalation_note {
        set_clauses.push("escalation_note = ?");
        params.push(nullable_text(e));
    }
    if let Some(ref rt) = overlay.required_tests {
        set_clauses.push("required_tests = ?");
        if rt.is_empty() {
            params.push(SqlValue::Null);
        } else {
            let encoded = serde_json::to_string(rt).map_err(|e| {
                TaskMgrError::invalid_state(
                    "update",
                    "requiredTests",
                    "JSON-encodable array",
                    e.to_string(),
                )
            })?;
            params.push(SqlValue::Text(encoded));
        }
    }
    if let Some(mr) = overlay.max_retries {
        set_clauses.push("max_retries = ?");
        params.push(SqlValue::Integer(i64::from(mr)));
    }
    if let Some(rh) = overlay.requires_human {
        set_clauses.push("requires_human = ?");
        params.push(SqlValue::Integer(i64::from(rh)));
    }
    if let Some(ref ht) = overlay.human_review_timeout {
        set_clauses.push("human_review_timeout = ?");
        params.push(nullable_u32(ht));
    }
    if let Some(ref csi) = overlay.claims_shared_infra {
        set_clauses.push("claims_shared_infra = ?");
        params.push(nullable_bool_i64(csi));
    }
    if let Some(ref rs) = overlay.review_scope {
        set_clauses.push("review_scope = ?");
        match rs {
            Nullable::Null => params.push(SqlValue::Null),
            Nullable::Present(v) => {
                let encoded = serde_json::to_string(v).map_err(|e| {
                    TaskMgrError::invalid_state(
                        "update",
                        "reviewScope",
                        "JSON-encodable value",
                        e.to_string(),
                    )
                })?;
                params.push(SqlValue::Text(encoded));
            }
        }
    }
    if let Some(ref s) = overlay.severity {
        set_clauses.push("severity = ?");
        params.push(nullable_text(s));
    }
    if let Some(ref sr) = overlay.source_review {
        set_clauses.push("source_review = ?");
        params.push(nullable_text(sr));
    }

    let mutates_tables = overlay.depends_on.is_some() || overlay.touches_files.is_some();
    let needs_task_update = !set_clauses.is_empty() || mutates_tables;

    if needs_task_update {
        set_clauses.push("updated_at = datetime('now')");
        let sql = format!("UPDATE tasks SET {} WHERE id = ?", set_clauses.join(", "));
        params.push(SqlValue::Text(task_id.to_string()));
        tx.execute(&sql, rusqlite::params_from_iter(params))?;
    }

    if let Some(ref deps) = overlay.depends_on {
        // Scoped delete — never delete_task_relationships (wipes synergy/…).
        tx.execute(
            "DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'",
            [task_id],
        )?;
        for dep in deps {
            let related = if prefix.is_empty() {
                dep.clone()
            } else {
                prefix_id(prefix, dep)
            };
            insert_relationship(&tx, task_id, &related, "dependsOn")?;
        }
    }

    if let Some(ref files) = overlay.touches_files {
        delete_task_files(&tx, task_id)?;
        for f in files {
            insert_task_file(&tx, task_id, f)?;
        }
    }

    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err_string(overlay: Value) -> String {
        validate_update_overlay(&overlay)
            .expect_err("expected reject")
            .to_string()
    }

    fn assert_invalid_update(overlay: Value, needle: &str) {
        let msg = err_string(overlay);
        assert!(
            msg.contains("Invalid state for update"),
            "expected invalid_state(\"update\", …), got: {msg}"
        );
        assert!(
            msg.contains(needle),
            "expected message to contain {needle:?}, got: {msg}"
        );
    }

    #[test]
    fn rejects_non_object() {
        assert_invalid_update(json!([]), "JSON object");
        assert_invalid_update(json!("x"), "JSON object");
        assert_invalid_update(json!(1), "JSON object");
        assert_invalid_update(Value::Null, "JSON object");
        assert_invalid_update(json!(true), "JSON object");
    }

    #[test]
    fn lifecycle_status_or_passes_before_unknown_and_type() {
        // Full blob with passes:false + valid notes → lifecycle, not unknown/type.
        let msg = err_string(json!({
            "id": "FEAT-001",
            "passes": false,
            "notes": "ok",
            "priority": 1,
            "foo": true
        }));
        assert!(msg.contains("Invalid state for update"), "{msg}");
        assert!(
            msg.contains("complete") && msg.contains("fail") && msg.contains("skip"),
            "lifecycle pointer missing: {msg}"
        );
        assert!(msg.contains("<task-status>"), "{msg}");
        assert!(msg.contains("passes"), "{msg}");
        assert!(
            !msg.contains("unknown keys"),
            "must not fall through to unknown-key: {msg}"
        );

        // status:done alone
        assert_invalid_update(json!({"id": "FEAT-001", "status": "done"}), "status");

        // top-level passes: null alone
        assert_invalid_update(json!({"id": "FEAT-001", "passes": null}), "passes");
    }

    #[test]
    fn unknown_keys_name_all_no_partial_apply() {
        let msg = err_string(json!({
            "id": "FEAT-001",
            "notes": "keep",
            "priority": 1,
            "foo": true,
            "synergyWith": ["A"],
            "batchWith": ["B"],
            "conflictsWith": ["C"],
            "newId": "X"
        }));
        assert!(msg.contains("unknown keys"), "{msg}");
        for k in [
            "priority",
            "foo",
            "synergyWith",
            "batchWith",
            "conflictsWith",
            "newId",
        ] {
            assert!(msg.contains(k), "missing {k} in: {msg}");
        }
        assert!(
            !msg.contains("coming soon"),
            "priority must not be a coming-soon message: {msg}"
        );
    }

    #[test]
    fn both_effort_aliases_ambiguous() {
        assert_invalid_update(
            json!({
                "id": "FEAT-001",
                "estimatedEffort": "high",
                "difficulty": "low"
            }),
            "ambiguous",
        );
        // Any values — including nulls — still ambiguous.
        assert_invalid_update(
            json!({
                "id": "FEAT-001",
                "estimatedEffort": null,
                "difficulty": null
            }),
            "ambiguous",
        );
    }

    #[test]
    fn nested_passes_inside_human_review_outcome_allowed() {
        let v = validate_update_overlay(&json!({
            "id": "CLARIFY-001",
            "humanReviewOutcome": {
                "passes": false,
                "resolvedAt": "2026-09-19",
                "resolvedBy": "op"
            }
        }))
        .expect("nested passes must be accepted");
        match v.human_review_outcome {
            Some(Nullable::Present(Value::Object(m))) => {
                assert_eq!(m.get("passes"), Some(&json!(false)));
            }
            other => panic!("expected Present(object), got {other:?}"),
        }
    }

    #[test]
    fn id_only_succeeds() {
        let v = validate_update_overlay(&json!({"id": "FEAT-001"})).expect("id-only ok");
        assert_eq!(v.id, "FEAT-001");
        assert!(v.title.is_none());
        assert!(v.notes.is_none());
        assert!(v.human_review_outcome.is_none());
    }

    #[test]
    fn title_null_or_empty_rejects() {
        assert_invalid_update(json!({"id": "FEAT-001", "title": null}), "title");
        assert_invalid_update(json!({"id": "FEAT-001", "title": ""}), "empty string");
    }

    #[test]
    fn string_nullable_scalars_null_clear() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "notes": null,
            "description": null,
            "model": null
        }))
        .expect("null clears");
        assert_eq!(v.notes, Some(Nullable::Null));
        assert_eq!(v.description, Some(Nullable::Null));
        assert_eq!(v.model, Some(Nullable::Null));
    }

    #[test]
    fn string_arrays_empty_clears_null_errors() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "dependsOn": [],
            "touchesFiles": ["src/a.rs"]
        }))
        .expect("arrays ok");
        assert_eq!(v.depends_on.as_deref(), Some(&[][..]));
        assert_eq!(v.touches_files, Some(vec!["src/a.rs".to_string()]));

        assert_invalid_update(json!({"id": "FEAT-001", "dependsOn": null}), "dependsOn");
        assert_invalid_update(
            json!({"id": "FEAT-001", "acceptanceCriteria": [1]}),
            "acceptanceCriteria",
        );
    }

    #[test]
    fn requires_human_bool_only() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "requiresHuman": true
        }))
        .expect("bool ok");
        assert_eq!(v.requires_human, Some(true));

        assert_invalid_update(json!({"id": "FEAT-001", "requiresHuman": null}), "null");
        assert_invalid_update(json!({"id": "FEAT-001", "requiresHuman": 1}), "bool");
        assert_invalid_update(json!({"id": "FEAT-001", "requiresHuman": "true"}), "bool");
    }

    #[test]
    fn max_retries_integer_null_rejects() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "maxRetries": 5
        }))
        .expect("int ok");
        assert_eq!(v.max_retries, Some(5));

        assert_invalid_update(json!({"id": "FEAT-001", "maxRetries": null}), "null");
        assert_invalid_update(json!({"id": "FEAT-001", "maxRetries": 1.5}), "integer");
    }

    #[test]
    fn claims_shared_infra_bool_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "claimsSharedInfra": null
        }))
        .expect("null clears");
        assert_eq!(v.claims_shared_infra, Some(Nullable::Null));

        assert_invalid_update(
            json!({"id": "FEAT-001", "claimsSharedInfra": "true"}),
            "claimsSharedInfra",
        );
    }

    #[test]
    fn human_review_timeout_unsigned_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "humanReviewTimeout": 60
        }))
        .expect("u32 ok");
        assert_eq!(v.human_review_timeout, Some(Nullable::Present(60)));

        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewTimeout": -1}),
            "humanReviewTimeout",
        );
        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewTimeout": 1.5}),
            "humanReviewTimeout",
        );
    }

    #[test]
    fn review_scope_any_json_value_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "reviewScope": {"paths": ["src/"]}
        }))
        .expect("object ok");
        assert!(matches!(v.review_scope, Some(Nullable::Present(_))));

        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "reviewScope": ["a", 1, true]
        }))
        .expect("array ok — do not as_str()");
        assert!(matches!(
            v.review_scope,
            Some(Nullable::Present(Value::Array(_)))
        ));

        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "reviewScope": null
        }))
        .expect("null clears");
        assert_eq!(v.review_scope, Some(Nullable::Null));
    }

    #[test]
    fn human_review_outcome_object_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "humanReviewOutcome": null
        }))
        .expect("null removes key");
        assert_eq!(v.human_review_outcome, Some(Nullable::Null));

        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewOutcome": []}),
            "humanReviewOutcome",
        );
        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewOutcome": "x"}),
            "object or null",
        );
    }

    #[test]
    fn effort_alias_either_key_alone_ok() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "estimatedEffort": "high"
        }))
        .expect("estimatedEffort alone");
        assert_eq!(v.difficulty, Some(Nullable::Present("high".into())));

        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "difficulty": null
        }))
        .expect("difficulty null clears");
        assert_eq!(v.difficulty, Some(Nullable::Null));
    }

    #[test]
    fn missing_id_rejects() {
        assert_invalid_update(json!({"notes": "x"}), "id");
    }

    /// Known-bad discriminator: typed PrdUserStory parse would hide rejects.
    #[test]
    fn must_not_use_prd_user_story_deserialize() {
        // If someone switched to from_value::<PrdUserStory>, passes:false would
        // deserialize (default/ignored) and priority would be silently accepted.
        // This overlay must still take the lifecycle reject.
        let overlay = json!({
            "id": "FEAT-001",
            "title": "t",
            "priority": 5,
            "passes": false
        });
        let msg = err_string(overlay);
        assert!(msg.contains("passes") || msg.contains("status"), "{msg}");
        assert!(msg.contains("complete"), "{msg}");
    }

    // -----------------------------------------------------------------------
    // FEAT-004: update_with_conn load-merge-write
    // -----------------------------------------------------------------------

    use crate::commands::init::import::{insert_relationship, insert_task_file, update_task};
    use crate::commands::init::parse::PrdUserStory;
    use crate::db::migrations::run_migrations;
    use crate::db::schema::create_schema;
    use rusqlite::Connection;
    use std::fs;
    use std::path::PathBuf;

    fn memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn seed_task(conn: &Connection, id: &str, title: &str, priority: i32, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status, notes) VALUES (?, ?, ?, ?, 'seed-notes')",
            rusqlite::params![id, title, priority, status],
        )
        .unwrap();
    }

    fn write_prd(path: &std::path::Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    fn read_story(path: &std::path::Path, id: &str) -> Value {
        let root: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        root["userStories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"].as_str() == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("story {id} not found"))
    }

    fn assert_invalid_update_cmd(err: &TaskMgrError) {
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "expected invalid_state(\"update\", …), got: {msg}"
        );
    }

    fn assert_no_export(msg: &str) {
        assert!(
            !msg.to_lowercase().contains("export"),
            "must never name export: {msg}"
        );
    }

    #[test]
    fn id_only_errors_before_writes() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-001", "t", 50, "todo");
        let err = update_with_conn(&conn, &json!({"id": "FEAT-001"}), None, "").unwrap_err();
        assert_invalid_update_cmd(&err);
        assert!(err.to_string().contains("no updatable fields"), "{}", err);
    }

    #[test]
    fn unknown_task_id_errors_before_writes() {
        let conn = memory_db();
        let err = update_with_conn(&conn, &json!({"id": "MISSING-001", "notes": "x"}), None, "")
            .unwrap_err();
        assert_invalid_update_cmd(&err);
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn notes_only_preserves_title_priority_status_archived_at() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-010", "KeepTitle", 42, "todo");
        // Learning #1211: seed archived_at via SQL UPDATE in tests.
        conn.execute(
            "UPDATE tasks SET archived_at = '2026-01-01T00:00:00Z' WHERE id = 'FEAT-010'",
            [],
        )
        .unwrap();

        let res = update_with_conn(
            &conn,
            &json!({"id": "FEAT-010", "notes": "new notes"}),
            None,
            "",
        )
        .unwrap();
        assert_eq!(res.fields_updated, vec!["notes".to_string()]);
        assert!(res.prd_path.is_none());

        let (title, priority, status, notes, archived): (
            String,
            i32,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT title, priority, status, notes, archived_at FROM tasks WHERE id = 'FEAT-010'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(title, "KeepTitle");
        assert_eq!(priority, 42);
        assert_eq!(status, "todo");
        assert_eq!(notes.as_deref(), Some("new notes"));
        assert_eq!(archived.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn in_progress_notes_only_keeps_status() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-011", "t", 10, "in_progress");
        update_with_conn(&conn, &json!({"id": "FEAT-011", "notes": "mid"}), None, "").unwrap();
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-011'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn either_effort_key_sets_difficulty_column() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-012", "t", 1, "todo");
        update_with_conn(
            &conn,
            &json!({"id": "FEAT-012", "estimatedEffort": "high"}),
            None,
            "",
        )
        .unwrap();
        let d: Option<String> = conn
            .query_row(
                "SELECT difficulty FROM tasks WHERE id = 'FEAT-012'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(d.as_deref(), Some("high"));

        update_with_conn(
            &conn,
            &json!({"id": "FEAT-012", "difficulty": "low"}),
            None,
            "",
        )
        .unwrap();
        let d: Option<String> = conn
            .query_row(
                "SELECT difficulty FROM tasks WHERE id = 'FEAT-012'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(d.as_deref(), Some("low"));
    }

    #[test]
    fn depends_on_scoped_delete_preserves_synergy_batch_conflicts() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-020", "t", 1, "todo");
        seed_task(&conn, "FEAT-021", "dep", 2, "todo");
        seed_task(&conn, "FEAT-022", "other", 3, "todo");
        insert_relationship(&conn, "FEAT-020", "FEAT-021", "dependsOn").unwrap();
        insert_relationship(&conn, "FEAT-020", "FEAT-022", "synergyWith").unwrap();
        insert_relationship(&conn, "FEAT-020", "FEAT-022", "batchWith").unwrap();
        insert_relationship(&conn, "FEAT-020", "FEAT-022", "conflictsWith").unwrap();

        // Replace dependsOn with empty — clears dependsOn only.
        update_with_conn(&conn, &json!({"id": "FEAT-020", "dependsOn": []}), None, "").unwrap();

        let dep_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'FEAT-020' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dep_count, 0);

        for rel in ["synergyWith", "batchWith", "conflictsWith"] {
            let c: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'FEAT-020' AND rel_type = ?",
                    [rel],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(c, 1, "{rel} must survive scoped dependsOn delete");
        }

        // Omit dependsOn → leave remaining rows (synergy/batch/conflicts).
        update_with_conn(
            &conn,
            &json!({"id": "FEAT-020", "notes": "leave-rels"}),
            None,
            "",
        )
        .unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'FEAT-020'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 3, "omit dependsOn must not touch relationships");

        // Replace with a new dep.
        update_with_conn(
            &conn,
            &json!({"id": "FEAT-020", "dependsOn": ["FEAT-021"]}),
            None,
            "",
        )
        .unwrap();
        let dep_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'FEAT-020' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dep_count, 1);
        let synergy: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'FEAT-020' AND rel_type = 'synergyWith'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(synergy, 1);
    }

    #[test]
    fn touches_files_omit_vs_replace_including_empty_clear() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-030", "t", 1, "todo");
        insert_task_file(&conn, "FEAT-030", "src/a.rs").unwrap();
        insert_task_file(&conn, "FEAT-030", "src/b.rs").unwrap();

        // Omit → leave files.
        update_with_conn(&conn, &json!({"id": "FEAT-030", "notes": "n"}), None, "").unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_files WHERE task_id = 'FEAT-030'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);

        // Replace.
        update_with_conn(
            &conn,
            &json!({"id": "FEAT-030", "touchesFiles": ["src/c.rs"]}),
            None,
            "",
        )
        .unwrap();
        let files: Vec<String> = conn
            .prepare(
                "SELECT file_path FROM task_files WHERE task_id = 'FEAT-030' ORDER BY file_path",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(files, vec!["src/c.rs".to_string()]);

        // [] clears.
        update_with_conn(
            &conn,
            &json!({"id": "FEAT-030", "touchesFiles": []}),
            None,
            "",
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_files WHERE task_id = 'FEAT-030'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn json_only_empty_path_is_invalid_state_no_db_write() {
        let conn = memory_db();
        seed_task(&conn, "CLARIFY-001", "t", 1, "todo");
        let before: Option<String> = conn
            .query_row(
                "SELECT updated_at FROM tasks WHERE id = 'CLARIFY-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let err = update_with_conn(
            &conn,
            &json!({
                "id": "CLARIFY-001",
                "humanReviewOutcome": {"resolvedAt": "2026-09-19", "resolvedBy": "op"}
            }),
            None,
            "",
        )
        .unwrap_err();
        assert_invalid_update_cmd(&err);
        let msg = err.to_string();
        assert!(msg.contains("task-mgr current"), "{msg}");
        assert!(msg.contains("--from-json"), "{msg}");
        assert_no_export(&msg);

        let after: Option<String> = conn
            .query_row(
                "SELECT updated_at FROM tasks WHERE id = 'CLARIFY-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, after, "JSON-only refuse must not UPDATE tasks");
    }

    #[test]
    fn json_only_empty_pathbuf_is_invalid_state() {
        let conn = memory_db();
        seed_task(&conn, "CLARIFY-002", "t", 1, "todo");
        let empty = PathBuf::new();
        let err = update_with_conn(
            &conn,
            &json!({"id": "CLARIFY-002", "humanReviewOutcome": null}),
            Some(empty.as_path()),
            "",
        )
        .unwrap_err();
        assert_invalid_update_cmd(&err);
        assert!(err.to_string().contains("task-mgr current"), "{err}");
    }

    #[test]
    fn json_only_patch_err_is_invalid_state_no_db_write() {
        let conn = memory_db();
        seed_task(&conn, "CLARIFY-003", "t", 1, "todo");
        // File exists but story id missing → patch Err.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            tmp.path(),
            r#"{"userStories":[{"id":"OTHER-001","title":"x","priority":1,"passes":false}]}"#,
        );

        let err = update_with_conn(
            &conn,
            &json!({
                "id": "CLARIFY-003",
                "humanReviewOutcome": {"resolvedBy": "op"}
            }),
            Some(tmp.path()),
            "",
        )
        .unwrap_err();
        assert_invalid_update_cmd(&err);
        let msg = err.to_string();
        assert!(msg.contains("task-mgr current"), "{msg}");
        assert!(msg.contains("--from-json"), "{msg}");
        assert_no_export(&msg);
    }

    #[test]
    fn mixed_empty_path_pin11_db_committed() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-040", "t", 1, "todo");
        let res = update_with_conn(
            &conn,
            &json!({
                "id": "FEAT-040",
                "notes": "db-ok",
                "humanReviewOutcome": {"resolvedBy": "op"}
            }),
            None,
            "",
        )
        .unwrap();
        assert!(res.prd_path.is_none());
        let notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'FEAT-040'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notes.as_deref(), Some("db-ok"));
    }

    #[test]
    fn mixed_patch_err_pin11_db_committed_never_export() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-041", "t", 1, "todo");
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            tmp.path(),
            r#"{"userStories":[{"id":"OTHER","title":"x","priority":1,"passes":false}]}"#,
        );

        let res = update_with_conn(
            &conn,
            &json!({
                "id": "FEAT-041",
                "notes": "committed",
                "humanReviewOutcome": {"resolvedBy": "op"}
            }),
            Some(tmp.path()),
            "",
        )
        .unwrap();
        assert!(res.prd_path.is_none(), "patch miss → pin 11 skip path");
        let notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'FEAT-041'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notes.as_deref(), Some("committed"));
        // Recovery copy is on stderr via ui::emit_err — spot-check helper text.
        let sample = format_mixed_json_failure(
            "FEAT-041",
            tmp.path(),
            &TaskMgrError::invalid_state("update", "userStory", "present", "not found"),
        );
        assert!(sample.contains("task-mgr current"), "{sample}");
        assert!(sample.contains("--from-json"), "{sample}");
        assert!(
            sample.contains("loop init --append --update-existing"),
            "{sample}"
        );
        assert_no_export(&sample);
    }

    #[test]
    fn json_only_success_persists_outcome_no_db_column() {
        let conn = memory_db();
        seed_task(&conn, "CLARIFY-010", "t", 1, "todo");
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            tmp.path(),
            r#"{
  "userStories": [
    {
      "id": "CLARIFY-010",
      "title": "clarify",
      "priority": 1,
      "passes": false,
      "requiresHuman": true
    }
  ]
}
"#,
        );

        let before_updated: Option<String> = conn
            .query_row(
                "SELECT updated_at FROM tasks WHERE id = 'CLARIFY-010'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let res = update_with_conn(
            &conn,
            &json!({
                "id": "CLARIFY-010",
                "humanReviewOutcome": {
                    "resolvedAt": "2026-09-19",
                    "resolvedBy": "op",
                    "confirmedValues": {"floor": 2}
                }
            }),
            Some(tmp.path()),
            "",
        )
        .unwrap();
        assert_eq!(res.prd_path.as_deref(), Some(tmp.path()));
        assert_eq!(res.fields_updated, vec!["humanReviewOutcome".to_string()]);

        let entry = read_story(tmp.path(), "CLARIFY-010");
        assert_eq!(
            entry["humanReviewOutcome"]["resolvedBy"].as_str(),
            Some("op")
        );
        assert_eq!(entry["id"].as_str(), Some("CLARIFY-010"));

        // No tasks.human_review_outcome column (pin 17).
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(tasks)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            !cols.iter().any(|c| c == "human_review_outcome"),
            "must not invent a DB column: {cols:?}"
        );

        // JSON-only issued no UPDATE tasks.
        let after_updated: Option<String> = conn
            .query_row(
                "SELECT updated_at FROM tasks WHERE id = 'CLARIFY-010'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before_updated, after_updated);

        // Re-import via import::update_task (loop init --append --update-existing
        // SQL path): field rides unused; JSON file still contains the object.
        let story: PrdUserStory = serde_json::from_value(entry.clone()).unwrap();
        assert!(story.human_review_outcome.is_some());
        update_task(&conn, &story, None).unwrap();
        let entry_after = read_story(tmp.path(), "CLARIFY-010");
        assert!(
            entry_after.get("humanReviewOutcome").is_some(),
            "JSON must still contain humanReviewOutcome after update_task"
        );
        assert_eq!(
            entry_after["humanReviewOutcome"]["resolvedBy"].as_str(),
            Some("op")
        );
    }

    #[test]
    fn empty_prefix_skips_prefix_id_on_lookup_and_depends_on() {
        let conn = memory_db();
        // Bare ids (NULL-prefix / --no-prefix).
        seed_task(&conn, "FEAT-050", "t", 1, "todo");
        seed_task(&conn, "FEAT-051", "dep", 2, "todo");

        update_with_conn(
            &conn,
            &json!({"id": "FEAT-050", "dependsOn": ["FEAT-051"], "notes": "n"}),
            None,
            "", // empty prefix
        )
        .unwrap();

        let related: String = conn
            .query_row(
                "SELECT related_id FROM task_relationships WHERE task_id = 'FEAT-050' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(related, "FEAT-051");
        // Must not invent "-FEAT-050" / "-FEAT-051".
        let bad: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE '-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(bad, 0);
    }

    #[test]
    fn non_empty_prefix_applies_to_lookup_and_depends_on_sql() {
        let conn = memory_db();
        seed_task(&conn, "pfx-FEAT-060", "t", 1, "todo");
        seed_task(&conn, "pfx-FEAT-061", "dep", 2, "todo");

        update_with_conn(
            &conn,
            &json!({"id": "FEAT-060", "dependsOn": ["FEAT-061"]}),
            None,
            "pfx",
        )
        .unwrap();

        let related: String = conn
            .query_row(
                "SELECT related_id FROM task_relationships WHERE task_id = 'pfx-FEAT-060' AND rel_type = 'dependsOn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(related, "pfx-FEAT-061");
    }

    #[test]
    fn mixed_notes_and_json_patch_success() {
        let conn = memory_db();
        seed_task(&conn, "FEAT-070", "Keep", 9, "todo");
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            tmp.path(),
            r#"{"userStories":[{"id":"FEAT-070","title":"Keep","priority":9,"passes":false,"notes":"old"}]}"#,
        );

        let res = update_with_conn(
            &conn,
            &json!({
                "id": "FEAT-070",
                "notes": "new",
                "humanReviewOutcome": {"resolvedBy": "op"}
            }),
            Some(tmp.path()),
            "",
        )
        .unwrap();
        assert_eq!(res.prd_path.as_deref(), Some(tmp.path()));
        let notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'FEAT-070'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notes.as_deref(), Some("new"));
        let entry = read_story(tmp.path(), "FEAT-070");
        assert_eq!(entry["notes"].as_str(), Some("new"));
        assert_eq!(
            entry["humanReviewOutcome"]["resolvedBy"].as_str(),
            Some("op")
        );
        assert_eq!(entry["title"].as_str(), Some("Keep"));
        assert_eq!(entry["priority"].as_i64(), Some(9));
    }

    // -----------------------------------------------------------------------
    // FEAT-005: pin / write-policy wrapper (update_with_roots)
    // -----------------------------------------------------------------------

    use crate::commands::context::{
        load_known_prefixes, preflight_from_json_path, refuse_unpinned_write, resolve_context,
    };
    use crate::loop_engine::claude::ACTIVE_PREFIX_ENV;

    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }
    impl EnvVarGuard {
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

    /// Field order load-bearing: restore env before releasing the mutex.
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

    fn seed_prefix(conn: &Connection, id: i64, project: &str, prefix: &str) {
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (?, ?, ?)",
            rusqlite::params![id, project, prefix],
        )
        .unwrap();
    }

    fn seed_task_list(conn: &Connection, prd_id: i64, path: &Path) {
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (?, ?, 'task_list')",
            rusqlite::params![prd_id, path.to_str().unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn directory_from_json_fails_before_parse_names_update() {
        // canonicalize on a dir succeeds — preflight is_file must run first.
        let tmp = tempfile::TempDir::new().unwrap();
        let err = preflight_from_json_path(tmp.path(), "update").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "directory preflight must name update (not add): {msg}"
        );
        assert!(
            msg.contains("not a regular file") || msg.contains("directory"),
            "{msg}"
        );
        // Garbage overlay would also fail parse — prove we never get there
        // when calling update() with a directory pin.
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).unwrap();
        // open_connection needs a migrated db; use update() only for the
        // preflight short-circuit (before parse / before open).
        let err = update(
            &db_dir,
            "this is not json {{{",
            Some(tmp.path()), // directory
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "update() directory pin must name update: {msg}"
        );
        assert!(
            !msg.contains("parse error"),
            "directory must fail before overlay parse: {msg}"
        );
    }

    #[test]
    fn refuse_ge2_unpinned_names_update_no_db_write() {
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        seed_task(&conn, "A-FEAT-001", "t", 1, "todo");

        assert!(resolve_context(&conn, None, "update").unwrap().is_none());
        refuse_unpinned_write(&conn, &None, "update").unwrap_err();

        let err = update_with_roots(
            &conn,
            &json!({"id": "FEAT-001", "notes": "x"}),
            None,
            None,
            None,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "≥2 refuse must name update: {msg}"
        );
        assert!(msg.contains("--from-json"), "{msg}");
        let notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'A-FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notes.as_deref(), Some("seed-notes"), "must not write");
    }

    #[test]
    fn zero_prefix_update_unprefixed_id_ok() {
        let _iso = isolate_env();
        let conn = memory_db();
        assert!(load_known_prefixes(&conn).unwrap().is_empty());
        seed_task(&conn, "FEAT-080", "t", 1, "todo");

        let res = update_with_roots(
            &conn,
            &json!({"id": "FEAT-080", "notes": "zero"}),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(res.task_id, "FEAT-080");
        let notes: Option<String> = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'FEAT-080'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notes.as_deref(), Some("zero"));
    }

    #[test]
    fn unregistered_from_json_after_parse_names_loop_init() {
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("orphan.json");
        write_prd(&file, r#"{"taskPrefix":"ORPHAN","userStories":[]}"#);
        let conn = memory_db();
        seed_task(&conn, "ORPHAN-FEAT-001", "t", 1, "todo");

        // Overlay is valid — error is registration, after parse, before write.
        let err = update_with_roots(
            &conn,
            &json!({"id": "FEAT-001", "notes": "x"}),
            Some(file.as_path()),
            Some(tmp.path()),
            Some(tmp.path()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "unregistered must name update: {msg}"
        );
        assert!(
            msg.contains("loop init"),
            "unregistered must name loop init: {msg}"
        );
        let notes: Option<String> = conn
            .query_row(
                "SELECT notes FROM tasks WHERE id = 'ORPHAN-FEAT-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(notes.as_deref(), Some("seed-notes"));
    }

    #[test]
    fn from_json_writes_canonical_path_not_remapped() {
        let _iso = isolate_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let tasks = tmp.path().join("tasks");
        fs::create_dir_all(&tasks).unwrap();
        let file = tasks.join("prd.json");
        write_prd(
            &file,
            r#"{"taskPrefix":"PIN","userStories":[{"id":"FEAT-090","title":"t","priority":1,"passes":false}]}"#,
        );
        let conn = memory_db();
        seed_prefix(&conn, 1, "pin", "PIN");
        seed_task_list(&conn, 1, &file);
        seed_task(&conn, "PIN-FEAT-090", "t", 1, "todo");

        let res = update_with_roots(
            &conn,
            &json!({"id": "FEAT-090", "notes": "pinned"}),
            Some(file.as_path()),
            Some(tmp.path()),
            Some(tmp.path()),
        )
        .unwrap();
        let expected = file.canonicalize().unwrap();
        assert_eq!(
            res.prd_path.as_ref().map(|p| p.canonicalize().unwrap()),
            Some(expected),
            "--from-json must write the canonical flag PATH"
        );
        // Must not insert extra prd_files / prd_metadata rows.
        let n_meta: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        let n_files: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_meta, 1);
        assert_eq!(n_files, 1);
    }

    #[test]
    fn format_text_mentions_fields_and_sync() {
        let text = format_text(&UpdateResult {
            task_id: "FEAT-001".into(),
            fields_updated: vec!["notes".into()],
            prd_path: Some(PathBuf::from("tasks/x.json")),
        });
        assert!(text.contains("FEAT-001"), "{text}");
        assert!(text.contains("notes"), "{text}");
        assert!(text.contains("tasks/x.json"), "{text}");
    }
}
