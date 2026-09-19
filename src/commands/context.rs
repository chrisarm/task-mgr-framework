//! Shared pin / active-PRD context resolver (CONTRACT-002).
//!
//! Owns `ResolvedContext`, `ResolutionSource`, `resolve_context`,
//! `resolve_active_prefix`, `locate_prd_json`, `load_known_prefixes`, the
//! pin-19 path identity helper (`paths_identify`), and write-only helpers
//! [`refuse_unpinned_write`] / [`preflight_from_json_path`] / [`default_prd_roots`].
//! `--from-json` registration is `(a)` JSON `taskPrefix` ∈ `prd_metadata`
//! **OR** pin-19 `(b)/(c)`.
//!
//! `resolve_context` keeps `Ok(None)` for **both** 0 and 2+ prefixes (so
//! `current` stays a probe). The ≥2-prefix refuse is **write-only** (add and
//! update) via [`refuse_unpinned_write`] — never inside `resolve_context`.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::git::{main_repo_root, remap_into_worktree, worktree_root};

/// Pin-19 (b)+(c) lives in [`crate::git::paths_identify`] (sticky SSoT).
pub use crate::git::paths_identify;
use crate::output::ui;
use crate::{TaskMgrError, TaskMgrResult};

/// How the active prefix / write target was resolved.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    /// `TASK_MGR_ACTIVE_PREFIX` env (non-empty + registered).
    EnvVar,
    /// Exactly one non-NULL `prd_metadata.task_prefix` and no env / flag.
    SinglePrefix,
    /// Explicit `--from-json PATH` pin (already-registered effort). NOT reserved.
    FromJsonFlag,
    /// Optional serde/compat variant; `resolve_context` returns `Option` so
    /// callers see `Ok(None)` rather than `Some(source=None)` for unpinned
    /// 0/2+ cases.
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
    /// Active effort prefix. Empty string when the matched `prd_metadata` row
    /// has NULL `task_prefix` (NULL-prefix / `--no-prefix` registered file).
    /// Callers MUST skip `apply_prefix` AND `prefix_id` when this is empty
    /// (otherwise `prefix_id("", "FEAT-001")` → `"-FEAT-001"`), and MUST pass
    /// `None` into `append_user_story` — never an id-shape prefix like
    /// `CODE` from `CODE-FIX-001` (that strips the JSON id to `FIX-001`).
    pub prefix: String,
    pub source: ResolutionSource,
    /// Write / display target (same path). For `--from-json`, always the
    /// canonical flag PATH (pin 13 — never remapped away). For env /
    /// single-prefix, remap-then-`is_file()` (worktree else registered else
    /// `PathBuf::new()` / skip). Display path == write path (learning #4237).
    pub prd_json_path: PathBuf,
}

/// Shared pin / active-PRD resolver for add, current, update, and export.
///
/// Precedence (CONTRACT-002): flag → env → exactly one non-NULL prefix →
/// `Ok(None)`. `Ok(None)` covers **both** 0 and 2+ prefixes (probe). The ≥2
/// refuse is **write-only** policy ([`refuse_unpinned_write`]) and must not
/// live here — `current` stays an `Ok(None)` probe.
///
/// `command` is the `invalid_state` command-name for every `Err` this function
/// (and its helpers) returns — never hardcode `"add"`. Logging / probe callers
/// pass `from_json: None`.
///
/// **Write-path callers** (`add` / `update`) must use
/// [`resolve_context_with_roots`] with [`default_prd_roots`] — do not call
/// bare `resolve_context` as the write-path resolver (TempDir / `--dir`
/// relative `prd_files` would remap onto the developer checkout).
pub fn resolve_context(
    conn: &Connection,
    from_json: Option<&Path>,
    command: &str,
) -> TaskMgrResult<Option<ResolvedContext>> {
    resolve_context_with_roots(conn, from_json, command, None, None)
}

/// Like [`resolve_context`], but `prd_files` reads use explicit project roots
/// (same pair as [`default_prd_roots`] / [`crate::commands::init::InitOpts::resolve_roots`]).
///
/// Write-path callers (`add` / `update`) pass `db_dir`-derived roots so TempDir
/// fixtures and source-root-relative `prd_files` rows resolve; cwd git roots
/// would remap those rows onto the developer checkout. Probe callers
/// (`current`) may pass `None` roots.
pub fn resolve_context_with_roots(
    conn: &Connection,
    from_json: Option<&Path>,
    command: &str,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<Option<ResolvedContext>> {
    // 1. FLAG — pin an already-registered effort (never remaps write target).
    if let Some(path) = from_json {
        return resolve_from_json_flag(conn, path, command, source_root, worktree_root).map(Some);
    }

    let env_value = std::env::var(crate::loop_engine::claude::ACTIVE_PREFIX_ENV).ok();
    let env_set = env_value.as_deref().is_some_and(|v| !v.is_empty());

    let prefix_opt = resolve_active_prefix(conn, command)?;

    let Some(prefix) = prefix_opt else {
        return Ok(None);
    };

    let source = if env_set {
        ResolutionSource::EnvVar
    } else {
        ResolutionSource::SinglePrefix
    };

    let registered = locate_prd_json(conn, Some(&prefix))?;
    let prd_json_path = match registered {
        Some(path) => match (source_root, worktree_root) {
            (Some(src), Some(wt)) => choose_cli_write_path(&path, src, wt),
            _ => cli_write_path(&path),
        },
        None => PathBuf::new(),
    };

    Ok(Some(ResolvedContext {
        prefix,
        source,
        prd_json_path,
    }))
}

/// Resolve `--from-json PATH` against registered efforts.
///
/// Order: missing → directory (!is_file) → canonicalize → registered iff
/// match (a) OR pin-19 (b)/(c) → else unregistered (names `loop init`).
/// On hit: `prd_json_path = canon` (pin 13); NULL `task_prefix` → empty
/// `prefix`. Never inserts `prd_files` / `prd_metadata`.
fn resolve_from_json_flag(
    conn: &Connection,
    path: &Path,
    command: &str,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<ResolvedContext> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(TaskMgrError::invalid_state(
                command,
                "--from-json",
                "an existing regular file path to a registered task_list",
                format!("path does not exist: {}", path.display()),
            ));
        }
        Err(e) => {
            return Err(TaskMgrError::io_error(
                path.display().to_string(),
                "reading --from-json path metadata",
                e,
            ));
        }
    };

    // Directory / non-file BEFORE canonicalize-as-parse (canonicalize succeeds
    // on directories — do not wait for a JSON parse error).
    if !meta.file_type().is_file() {
        return Err(TaskMgrError::invalid_state(
            command,
            "--from-json",
            "a regular file (already-registered task_list)",
            format!(
                "path is not a regular file (directory or special): {}",
                path.display()
            ),
        ));
    }

    let canon = fs::canonicalize(path).map_err(|e| {
        TaskMgrError::io_error(
            path.display().to_string(),
            "canonicalizing --from-json path",
            e,
        )
    })?;

    let matched_prefix = match_registered_from_json(conn, &canon, source_root, worktree_root)?;
    let Some(prefix_opt) = matched_prefix else {
        return Err(TaskMgrError::invalid_state(
            command,
            "--from-json",
            "a path registered as a task_list (run `task-mgr loop init <prd>.json` first)",
            format!(
                "{} is not a registered task_list; pin only works for already-registered efforts",
                canon.display()
            ),
        ));
    };

    let prefix = prefix_opt.unwrap_or_default();

    // Flag wins over env; note when they disagree, then proceed.
    let env_value = std::env::var(crate::loop_engine::claude::ACTIVE_PREFIX_ENV).ok();
    if let Some(env_prefix) = env_value.as_deref().filter(|v| !v.is_empty())
        && env_prefix != prefix
    {
        let shown = if prefix.is_empty() {
            "(none)".to_string()
        } else {
            prefix.clone()
        };
        ui::emit_err(&format!(
            "note: --from-json wins over {}={} (using pin prefix '{shown}')",
            crate::loop_engine::claude::ACTIVE_PREFIX_ENV,
            env_prefix,
        ));
    }

    Ok(ResolvedContext {
        prefix,
        source: ResolutionSource::FromJsonFlag,
        prd_json_path: canon,
    })
}

/// Registration OR: (a) JSON `taskPrefix` ∈ known non-NULL prefixes, else
/// pin-19 (b)/(c) against `prd_files` task_list rows.
///
/// Returns `Ok(None)` when unregistered; `Ok(Some(None))` for a NULL-prefix
/// row match; `Ok(Some(Some(prefix)))` for a named prefix.
fn match_registered_from_json(
    conn: &Connection,
    canon: &Path,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<Option<Option<String>>> {
    // Match (a): read taskPrefix off Value — do not deserialize stories.
    if let Some(json_prefix) = read_json_task_prefix(canon) {
        let known = load_known_prefixes(conn)?;
        if known.iter().any(|p| p == &json_prefix) {
            return Ok(Some(Some(json_prefix)));
        }
    }

    // Match (b)+(c): path identity against registered task_list rows.
    // Keep prefix-only at this layer — do not fold match (a) into identity.
    Ok(
        find_registered_by_path_identity(conn, canon, source_root, worktree_root)?
            .map(|(_prd_id, prefix)| prefix),
    )
}

/// Best-effort `taskPrefix` from the JSON file root. Parse failures / missing
/// key → `None` (caller still tries path identity).
fn read_json_task_prefix(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match &value["taskPrefix"] {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Pin-19 path identity against every `task_list` row.
///
/// Returns matched `(prd_files.prd_id, task_prefix)`. `task_prefix` is `None`
/// when the column is NULL (`--no-prefix` / empty prefix). Match (a) (JSON
/// `taskPrefix` ∈ known prefixes) is **not** this function —
/// [`match_registered_from_json`] keeps (a) OR identity and still exposes
/// prefix-only to [`resolve_context`].
pub(crate) fn find_registered_by_path_identity(
    conn: &Connection,
    flag_canon: &Path,
    source_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> TaskMgrResult<Option<(i64, Option<String>)>> {
    let source_root = source_root
        .map(Path::to_path_buf)
        .or_else(main_repo_root)
        .or_else(crate::git::worktree_root);
    let worktree = worktree_root
        .map(Path::to_path_buf)
        .or_else(crate::git::worktree_root)
        .or_else(|| source_root.clone());

    let mut stmt = conn.prepare(
        "SELECT pf.prd_id, pf.file_path, pm.task_prefix FROM prd_files pf \
         JOIN prd_metadata pm ON pf.prd_id = pm.id \
         WHERE pf.file_type = 'task_list'",
    )?;
    let rows = stmt.query_map([], |row| {
        let prd_id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let prefix: Option<String> = row.get(2)?;
        Ok((prd_id, PathBuf::from(path), prefix))
    })?;

    for row in rows {
        let (prd_id, registered, prefix) = row?;

        // Absolute registered path: canonicalize equality is sufficient and
        // does not depend on git roots (covers TempDir init fixtures).
        if registered.is_absolute()
            && registered
                .canonicalize()
                .ok()
                .is_some_and(|r| r == flag_canon)
        {
            return Ok(Some((prd_id, prefix)));
        }

        if let (Some(src), Some(wt)) = (&source_root, &worktree) {
            let src = src.canonicalize().unwrap_or_else(|_| src.clone());
            let wt = wt.canonicalize().unwrap_or_else(|_| wt.clone());
            if paths_identify(flag_canon, &registered, &src, &wt) {
                return Ok(Some((prd_id, prefix)));
            }
        } else if !registered.is_absolute() {
            // Non-git cwd fallback for relative rows (best-effort).
            if let Ok(cwd) = std::env::current_dir() {
                let cwd = cwd.canonicalize().unwrap_or(cwd);
                if paths_identify(flag_canon, &registered, &cwd, &cwd) {
                    return Ok(Some((prd_id, prefix)));
                }
            }
        }
    }
    Ok(None)
}

/// Returns all non-NULL `task_prefix` values from `prd_metadata`.
///
/// Single canonical home for the `WHERE task_prefix IS NOT NULL` query —
/// callers must not repeat this SQL directly. Used by the **write-only** ≥2
/// refuse predicate ([`refuse_unpinned_write`]) for add and update.
pub(crate) fn load_known_prefixes(conn: &Connection) -> TaskMgrResult<Vec<String>> {
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
///    as unset. `command` is forwarded into every stale-pin `invalid_state`.
/// 2. Env unset/empty → single-prefix fallback: `Ok(Some(prefix))` when
///    exactly one non-NULL `task_prefix` exists; `Ok(None)` otherwise (0 **or**
///    2+ prefixes — probe, not refuse).
///
/// DB errors propagate via `?` — an unexpected failure must not silently
/// bypass auto-prefixing. `std::env::var` is read exactly once at entry.
pub(crate) fn resolve_active_prefix(
    conn: &Connection,
    command: &str,
) -> TaskMgrResult<Option<String>> {
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
            command,
            crate::loop_engine::claude::ACTIVE_PREFIX_ENV,
            format!("a prefix registered in prd_metadata (known: {known_display})"),
            format!("{env_prefix} (not found in prd_metadata)"),
        ));
    }
    let prefixes = load_known_prefixes(conn)?;
    if prefixes.len() == 1 {
        Ok(Some(prefixes.into_iter().next().expect("len == 1")))
    } else {
        Ok(None)
    }
}

/// Look up the registered `task_list` path for a known prefix.
///
/// Prefix-scoped only — no `LIMIT 1` fallback on miss (FEAT-004b / learning
/// #2236). Sole-row lookup for the write-path `ctx is None` case lives in
/// [`sole_task_list_path`]. Callers that need the CLI write target must run
/// the result through [`cli_write_path`] / [`choose_cli_write_path`].
///
/// Returns `Ok(None)` when `task_prefix` is `None` or no matching `task_list`
/// row exists (valid: programmatic DB, foreign-prefix miss for refuse copy).
pub(crate) fn locate_prd_json(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> TaskMgrResult<Option<PathBuf>> {
    let Some(prefix) = task_prefix else {
        return Ok(None);
    };
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
    Ok(result.map(PathBuf::from))
}

/// Write-only ≥2-prefix refuse (add **and** update).
///
/// Predicate: `ctx.is_none() && load_known_prefixes().len() >= 2`. Keep this
/// **outside** [`resolve_context`] so `current` stays an `Ok(None)` probe.
/// Do **not** fold into `if ctx.is_none()` alone — that breaks zero-prefix /
/// `--no-prefix` writes (known.len() == 0).
///
/// `command` is the `invalid_state` command-name (`"add"` / `"update"`).
pub(crate) fn refuse_unpinned_write(
    conn: &Connection,
    ctx: &Option<ResolvedContext>,
    command: &str,
) -> TaskMgrResult<()> {
    if ctx.is_none() {
        let known = load_known_prefixes(conn)?;
        if known.len() >= 2 {
            return Err(TaskMgrError::invalid_state(
                command,
                "--from-json / TASK_MGR_ACTIVE_PREFIX",
                "pin via --from-json or TASK_MGR_ACTIVE_PREFIX",
                format!(
                    "{} registered prefixes ({}), none selected",
                    known.len(),
                    known.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

/// Filesystem-only preflight for `--from-json`: missing and non-file paths
/// fail **before** overlay / input JSON parse.
///
/// Registration matching stays in [`resolve_context_with_roots`] (needs DB)
/// but still precedes any write transaction. `command` is forwarded into every
/// `invalid_state` — never hardcode `"add"` (update missing/directory errors
/// must say `"update"`).
pub(crate) fn preflight_from_json_path(path: &Path, command: &str) -> TaskMgrResult<()> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(TaskMgrError::invalid_state(
                command,
                "--from-json",
                "an existing regular file path to a registered task_list",
                format!("path does not exist: {}", path.display()),
            ));
        }
        Err(e) => {
            return Err(TaskMgrError::io_error(
                path.display().to_string(),
                "reading --from-json path metadata",
                e,
            ));
        }
    };
    if !meta.file_type().is_file() {
        return Err(TaskMgrError::invalid_state(
            command,
            "--from-json",
            "a regular file (already-registered task_list)",
            format!(
                "path is not a regular file (directory or special): {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Same fallback as [`crate::commands::init::InitOpts::resolve_roots`].
///
/// Shared by write-path callers (`add` / `update`) so TempDir / `--dir`
/// relative `prd_files` resolve against the project, not the developer
/// checkout. Do not `use commands::add` from update — call this instead.
pub(crate) fn default_prd_roots(db_dir: &Path) -> (PathBuf, PathBuf) {
    let source_root = crate::git::main_repo_root_at(db_dir)
        .or_else(|| db_dir.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| db_dir.to_path_buf());
    let worktree_root = std::env::current_dir()
        .ok()
        .filter(|cwd| crate::git::is_inside_worktree_at(cwd).unwrap_or(false))
        .unwrap_or_else(|| source_root.clone());
    (source_root, worktree_root)
}

/// Exactly one registered `task_list` row, or `None`.
///
/// Write-only: used on the `ctx is None` JSON-sync path for add **and**
/// update (zero non-NULL prefixes / `--no-prefix`): sync iff count == 1 and
/// the chosen write path is a regular file. Never a prefix-miss fallback.
pub(crate) fn sole_task_list_path(conn: &Connection) -> TaskMgrResult<Option<PathBuf>> {
    let mut stmt = conn.prepare("SELECT file_path FROM prd_files WHERE file_type = 'task_list'")?;
    let paths: Vec<PathBuf> = stmt
        .query_map([], |row| {
            let p: String = row.get(0)?;
            Ok(PathBuf::from(p))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if paths.len() == 1 {
        Ok(paths.into_iter().next())
    } else {
        Ok(None)
    }
}

/// CLI write-path policy with explicit roots (unit-testable; no git probe).
///
/// `resolved = join-or-absolute(registered, source_root)`;
/// `target = remap_into_worktree(...)`;
/// then worktree file → registered file → `PathBuf::new()` (skip).
pub(crate) fn choose_cli_write_path(
    registered: &Path,
    source_root: &Path,
    worktree_root: &Path,
) -> PathBuf {
    let resolved = if registered.is_absolute() {
        registered.to_path_buf()
    } else {
        source_root.join(registered)
    };
    let target = remap_into_worktree(registered, source_root, worktree_root);
    if target.is_file() {
        target
    } else if resolved.is_file() {
        resolved
    } else {
        PathBuf::new()
    }
}

/// CLI write-path policy using live `main_repo_root` / `worktree_root`.
///
/// Non-git / TempDir fixtures (no roots): absolute registered stands alone;
/// relative joins cwd. Result is what default-path `ResolvedContext.prd_json_path`
/// holds — never remap `--from-json` through this.
pub(crate) fn cli_write_path(registered: &Path) -> PathBuf {
    let source_root = main_repo_root().or_else(worktree_root);
    let worktree = worktree_root().or_else(|| source_root.clone());
    match (source_root, worktree) {
        (Some(src), Some(wt)) => {
            let src = src.canonicalize().unwrap_or(src);
            let wt = wt.canonicalize().unwrap_or(wt);
            choose_cli_write_path(registered, &src, &wt)
        }
        _ => {
            let resolved = if registered.is_absolute() {
                registered.to_path_buf()
            } else if let Ok(cwd) = std::env::current_dir() {
                cwd.join(registered)
            } else {
                registered.to_path_buf()
            };
            if resolved.is_file() {
                resolved
            } else {
                PathBuf::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::schema::create_schema;
    use std::fs;
    use tempfile::TempDir;

    const ACTIVE_PREFIX_ENV: &str = crate::loop_engine::claude::ACTIVE_PREFIX_ENV;

    /// Process-wide mutex: env var manipulation is not safe across threads.
    /// Prefer the crate-level `ENV_PREFIX_MUTEX` when available so add/current
    /// tests serialize against the same lock.
    fn memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn seed_prefix(conn: &Connection, id: i64, project: &str, task_prefix: &str) {
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (?, ?, ?)",
            rusqlite::params![id, project, task_prefix],
        )
        .unwrap();
    }

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

    /// Field order is load-bearing: `_env` before `_lock` so restore runs
    /// before the mutex is released (learning #4883).
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
    fn isolate_env_set(value: &str) -> EnvIsolation {
        let lock = crate::ENV_PREFIX_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let env = EnvVarGuard::set(ACTIVE_PREFIX_ENV, value);
        EnvIsolation {
            _env: env,
            _lock: lock,
        }
    }

    // --- resolve_active_prefix / resolve_context (resolver, not write-policy) ---

    #[test]
    fn test_resolve_prefix_env_unset_single_prd_returns_some() {
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        let resolved = resolve_active_prefix(&conn, "add").unwrap();
        assert_eq!(resolved, Some("A-".to_string()));
    }

    #[test]
    fn test_resolve_prefix_env_unset_multi_prd_returns_none() {
        // Resolver probe: 2+ prefixes → Ok(None). Write-policy refuse is
        // refuse_unpinned_write (write-only: add and update) and must not
        // live inside resolve_context.
        let _iso = isolate_env();
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        seed_prefix(&conn, 2, "beta", "B-");
        let resolved = resolve_active_prefix(&conn, "add").unwrap();
        assert_eq!(resolved, None);
        let ctx = resolve_context(&conn, None, "add").unwrap();
        assert!(ctx.is_none(), "2+ prefixes must stay Ok(None) at resolver");
    }

    #[test]
    fn test_resolve_prefix_env_set_matching_multi_prd_returns_pinned() {
        let _iso = isolate_env_set("A-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        seed_prefix(&conn, 2, "beta", "B-");
        let resolved = resolve_active_prefix(&conn, "add").unwrap();
        assert_eq!(resolved, Some("A-".to_string()));
    }

    #[test]
    fn test_resolve_prefix_env_set_stale_errors_with_known_prefixes() {
        let _iso = isolate_env_set("stale-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        let err = resolve_active_prefix(&conn, "add").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("stale-"), "error must name stale value: {msg}");
        assert!(msg.contains("A-"), "error must list known prefixes: {msg}");
    }

    #[test]
    fn test_stale_pin_command_name_is_parameterized() {
        // Stale-pin from `current` must not blame `add`.
        let _iso = isolate_env_set("stale-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        let err = resolve_active_prefix(&conn, "current").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("current"),
            "stale-pin must name the calling command: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("\"add\"") && !msg.contains("command `add`"),
            "stale-pin from current must not say add: {msg}"
        );
    }

    #[test]
    fn test_resolve_prefix_env_empty_string_treated_as_unset() {
        let _iso = isolate_env_set("");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        seed_prefix(&conn, 2, "beta", "B-");
        let resolved = resolve_active_prefix(&conn, "add").unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn test_resolve_prefix_env_set_with_empty_metadata_errors() {
        let _iso = isolate_env_set("X-");
        let conn = memory_db();
        let err = resolve_active_prefix(&conn, "add").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("X-"), "error must name stale value: {msg}");
        assert!(
            msg.contains("none registered") || msg.contains("not found"),
            "error must indicate empty/missing prefix set: {msg}"
        );
    }

    #[test]
    fn test_resolve_prefix_naive_passthrough_would_fail_stale_check() {
        let _iso = isolate_env_set("B-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A-");
        assert!(
            resolve_active_prefix(&conn, "add").is_err(),
            "must reject pinned prefix not present in prd_metadata"
        );
    }

    #[test]
    fn test_resolve_context_zero_prefixes_ok_none() {
        let _iso = isolate_env();
        let conn = memory_db();
        let result = resolve_context(&conn, None, "current").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_from_json_flag_display_is_from_json() {
        assert_eq!(ResolutionSource::FromJsonFlag.to_string(), "from-json");
    }

    #[test]
    fn test_locate_prd_json_returns_none_when_no_file_registered() {
        let conn = memory_db();
        let path = locate_prd_json(&conn, None).unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn test_locate_prd_json_prefix_miss_does_not_limit1_fallback() {
        // Architect tightening: prefix-scoped only. A miss must not silently
        // append PRD #1 (learning #2236 / FEAT-004b).
        let conn = memory_db();
        seed_task_list(&conn, 1, "alpha", Some("A-"), Path::new("/tmp/alpha.json"));
        let miss = locate_prd_json(&conn, Some("B-")).unwrap();
        assert!(
            miss.is_none(),
            "prefix miss must not fall back to another PRD's task_list"
        );
        let hit = locate_prd_json(&conn, Some("A-")).unwrap();
        assert_eq!(hit, Some(PathBuf::from("/tmp/alpha.json")));
        assert!(
            locate_prd_json(&conn, None).unwrap().is_none(),
            "locate_prd_json(None) is not the sole-row path"
        );
    }

    #[test]
    fn test_sole_task_list_path_count_eq_1_only() {
        let conn = memory_db();
        assert!(sole_task_list_path(&conn).unwrap().is_none());
        seed_task_list(&conn, 1, "alpha", Some("A-"), Path::new("/tmp/alpha.json"));
        assert_eq!(
            sole_task_list_path(&conn).unwrap(),
            Some(PathBuf::from("/tmp/alpha.json"))
        );
        seed_task_list(&conn, 2, "beta", Some("B-"), Path::new("/tmp/beta.json"));
        assert!(
            sole_task_list_path(&conn).unwrap().is_none(),
            "count!=1 must skip (no LIMIT 1)"
        );
    }

    #[test]
    fn test_choose_cli_write_path_prefers_remapped_file() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(source.join("tasks")).unwrap();
        fs::create_dir_all(wt.join("tasks")).unwrap();
        // Both exist; remapped wins.
        fs::write(source.join("tasks/foo.json"), r#"{"main":true}"#).unwrap();
        fs::write(wt.join("tasks/foo.json"), r#"{"wt":true}"#).unwrap();
        let got = choose_cli_write_path(Path::new("tasks/foo.json"), &source, &wt);
        assert_eq!(got, wt.join("tasks/foo.json"));
    }

    #[test]
    fn test_choose_cli_write_path_falls_back_to_registered_file() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(source.join("tasks")).unwrap();
        fs::create_dir_all(wt.join("tasks")).unwrap();
        // Only registered (joined) exists.
        fs::write(source.join("tasks/foo.json"), r#"{"main":true}"#).unwrap();
        let got = choose_cli_write_path(Path::new("tasks/foo.json"), &source, &wt);
        assert_eq!(got, source.join("tasks/foo.json"));
    }

    #[test]
    fn test_choose_cli_write_path_neither_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(source.join("tasks")).unwrap();
        fs::create_dir_all(wt.join("tasks")).unwrap();
        let got = choose_cli_write_path(Path::new("tasks/foo.json"), &source, &wt);
        assert_eq!(got, PathBuf::new());
    }

    // --- pin-19 paths_identify (b)+(c) ---

    #[test]
    fn test_paths_identify_b_same_canonical_path() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("tasks");
        fs::create_dir_all(&file).unwrap();
        let prd = file.join("foo.json");
        fs::write(&prd, "{}").unwrap();
        let flag = prd.canonicalize().unwrap();
        let registered = Path::new("tasks/foo.json");
        assert!(paths_identify(
            &flag,
            registered,
            tmp.path(),
            tmp.path(), // main checkout: worktree == source
        ));
    }

    #[test]
    fn test_paths_identify_c_remapped_worktree_path() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(source.join("tasks")).unwrap();
        fs::create_dir_all(wt.join("tasks")).unwrap();
        // Only the worktree copy exists on disk (typical linked-worktree case
        // where the flag points at the worktree file).
        let wt_prd = wt.join("tasks/foo.json");
        fs::write(&wt_prd, "{}").unwrap();
        let flag = wt_prd.canonicalize().unwrap();
        let registered = Path::new("tasks/foo.json");
        assert!(
            paths_identify(&flag, registered, &source, &wt),
            "flag at worktree path must match via remap leg (c)"
        );
    }

    #[test]
    fn test_paths_identify_rejects_unrelated_path() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("tasks")).unwrap();
        let prd = tmp.path().join("tasks/foo.json");
        fs::write(&prd, "{}").unwrap();
        let other = tmp.path().join("tasks/other.json");
        fs::write(&other, "{}").unwrap();
        let flag = other.canonicalize().unwrap();
        assert!(!paths_identify(
            &flag,
            Path::new("tasks/foo.json"),
            tmp.path(),
            tmp.path(),
        ));
    }

    // --- FEAT-004: --from-json pin matrix ---

    fn seed_task_list(
        conn: &Connection,
        prd_id: i64,
        project: &str,
        task_prefix: Option<&str>,
        file_path: &Path,
    ) {
        conn.execute(
            "INSERT INTO prd_metadata (id, project, task_prefix) VALUES (?, ?, ?)",
            rusqlite::params![prd_id, project, task_prefix],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (?, ?, 'task_list')",
            rusqlite::params![prd_id, file_path.to_str().unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn test_from_json_happy_match_a_task_prefix() {
        let _iso = isolate_env();
        let tmp = TempDir::new().unwrap();
        let prd = tmp.path().join("tasks");
        fs::create_dir_all(&prd).unwrap();
        let file = prd.join("foo.json");
        fs::write(
            &file,
            r#"{"taskPrefix":"ALPHA","project":"p","userStories":[]}"#,
        )
        .unwrap();

        let conn = memory_db();
        // Absolute path as init would store when outside tasks_dir.
        seed_task_list(&conn, 1, "p", Some("ALPHA"), &file);

        let ctx = resolve_context(&conn, Some(&file), "add")
            .unwrap()
            .expect("registered pin must resolve");
        assert_eq!(ctx.source, ResolutionSource::FromJsonFlag);
        assert_eq!(ctx.prefix, "ALPHA");
        assert_eq!(ctx.prd_json_path, file.canonicalize().unwrap());
    }

    #[test]
    fn test_from_json_missing_path_errors_before_registration() {
        let _iso = isolate_env();
        let conn = memory_db();
        let missing = Path::new("/tmp/task-mgr-feat004-does-not-exist.json");
        let err = resolve_context(&conn, Some(missing), "add").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("does not exist") || msg.contains("path does not exist"),
            "missing copy must be distinct: {msg}"
        );
        assert!(
            !msg.contains("not a registered task_list"),
            "missing must not look like unregistered: {msg}"
        );
    }

    #[test]
    fn test_from_json_directory_errors_distinct_copy() {
        let _iso = isolate_env();
        let tmp = TempDir::new().unwrap();
        let conn = memory_db();
        let err = resolve_context(&conn, Some(tmp.path()), "add").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not a regular file") || msg.contains("directory"),
            "directory copy must be distinct: {msg}"
        );
        assert!(
            !msg.contains("not a registered task_list"),
            "directory must not look like unregistered: {msg}"
        );
        assert!(
            !msg.contains("does not exist"),
            "directory must not look like missing: {msg}"
        );
    }

    #[test]
    fn test_from_json_unregistered_names_loop_init() {
        let _iso = isolate_env();
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("never-inited.json");
        fs::write(&file, r#"{"taskPrefix":"ORPHAN","userStories":[]}"#).unwrap();
        let conn = memory_db();
        let err = resolve_context(&conn, Some(&file), "add").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("loop init"),
            "unregistered must name loop init: {msg}"
        );
        assert!(
            msg.contains("not a registered task_list"),
            "unregistered copy: {msg}"
        );
    }

    #[test]
    fn test_preflight_from_json_path_names_command() {
        let tmp = TempDir::new().unwrap();
        // Missing
        let missing = tmp.path().join("nope.json");
        let err = preflight_from_json_path(&missing, "update").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "missing must name update: {msg}"
        );
        assert!(msg.contains("does not exist"), "{msg}");

        // Directory (canonicalize would succeed — is_file before parse)
        let err = preflight_from_json_path(tmp.path(), "update").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "directory must name update: {msg}"
        );
        assert!(
            msg.contains("not a regular file") || msg.contains("directory"),
            "{msg}"
        );

        // Regular file OK
        let file = tmp.path().join("ok.json");
        fs::write(&file, "{}").unwrap();
        preflight_from_json_path(&file, "update").expect("regular file must pass");
    }

    #[test]
    fn test_refuse_unpinned_write_ge2_only() {
        let _iso = isolate_env();
        let conn = memory_db();

        // 0 prefixes + ctx None → allow
        refuse_unpinned_write(&conn, &None, "update").expect("0-prefix must allow");

        seed_prefix(&conn, 1, "alpha", "A-");
        // 1 prefix + ctx None → allow (single-prefix would normally resolve,
        // but refuse itself only gates ≥2)
        refuse_unpinned_write(&conn, &None, "update").expect("1-prefix must allow");

        seed_prefix(&conn, 2, "beta", "B-");
        let err = refuse_unpinned_write(&conn, &None, "update").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid state for update"),
            "≥2 refuse must name update: {msg}"
        );
        assert!(msg.contains("--from-json"), "{msg}");
        assert!(msg.contains("TASK_MGR_ACTIVE_PREFIX"), "{msg}");

        // Pinned ctx → allow even with ≥2
        let pinned = Some(ResolvedContext {
            prefix: "A-".to_string(),
            source: ResolutionSource::EnvVar,
            prd_json_path: PathBuf::new(),
        });
        refuse_unpinned_write(&conn, &pinned, "update").expect("pinned must allow");
    }

    #[test]
    fn test_from_json_null_prefix_returns_empty_prefix() {
        let _iso = isolate_env();
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("noprefix.json");
        fs::write(&file, r#"{"project":"p","userStories":[]}"#).unwrap();
        let conn = memory_db();
        seed_task_list(&conn, 1, "p", None, &file);

        let ctx = resolve_context(&conn, Some(&file), "add")
            .unwrap()
            .expect("NULL-prefix registered file must resolve Some");
        assert_eq!(ctx.source, ResolutionSource::FromJsonFlag);
        assert_eq!(ctx.prefix, "", "NULL task_prefix → empty string");
        assert_eq!(ctx.prd_json_path, file.canonicalize().unwrap());
    }

    #[test]
    fn test_from_json_flag_wins_over_env_mismatch() {
        let _iso = isolate_env_set("OTHER");
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("alpha.json");
        fs::write(
            &file,
            r#"{"taskPrefix":"ALPHA","project":"p","userStories":[]}"#,
        )
        .unwrap();
        let conn = memory_db();
        seed_task_list(&conn, 1, "p", Some("ALPHA"), &file);
        // Env points at a different registered prefix — flag must still win.
        seed_prefix(&conn, 2, "other", "OTHER");

        let ctx = resolve_context(&conn, Some(&file), "add")
            .unwrap()
            .expect("flag must win");
        assert_eq!(ctx.source, ResolutionSource::FromJsonFlag);
        assert_eq!(ctx.prefix, "ALPHA");
    }

    #[test]
    fn test_from_json_relative_prd_files_row_match_via_identity() {
        // Seed as init-shaped relative path `tasks/foo.json` (never bare
        // basename unless that is what init stored). Exercise pin-19 (b)
        // with source_root == worktree (main checkout).
        let _iso = isolate_env();
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("tasks")).unwrap();
        let file = tmp.path().join("tasks/foo.json");
        fs::write(&file, r#"{"project":"p","userStories":[]}"#).unwrap();

        let conn = memory_db();
        seed_task_list(&conn, 1, "p", Some("FOO"), Path::new("tasks/foo.json"));

        // Without git roots, absolute canonicalize equality won't hit the
        // relative row — drive identity through paths_identify directly, then
        // confirm resolve_context via match (a) when taskPrefix is present.
        assert!(paths_identify(
            &file.canonicalize().unwrap(),
            Path::new("tasks/foo.json"),
            tmp.path(),
            tmp.path(),
        ));

        // Match (a) path: put taskPrefix in the JSON so resolve_context hits
        // without needing git cwd == tmp.
        fs::write(
            &file,
            r#"{"taskPrefix":"FOO","project":"p","userStories":[]}"#,
        )
        .unwrap();
        let ctx = resolve_context(&conn, Some(&file), "add")
            .unwrap()
            .expect("relative+prefix must register via (a)");
        assert_eq!(ctx.source, ResolutionSource::FromJsonFlag);
        assert_eq!(ctx.prefix, "FOO");
        assert_eq!(ctx.prd_json_path, file.canonicalize().unwrap());
    }

    #[test]
    fn test_from_json_does_not_insert_prd_rows() {
        let _iso = isolate_env();
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("alpha.json");
        fs::write(
            &file,
            r#"{"taskPrefix":"ALPHA","project":"p","userStories":[]}"#,
        )
        .unwrap();
        let conn = memory_db();
        seed_task_list(&conn, 1, "p", Some("ALPHA"), &file);

        let meta_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        let files_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
            .unwrap();

        let _ = resolve_context(&conn, Some(&file), "add").unwrap();

        let meta_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        let files_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(meta_before, meta_after, "must not insert prd_metadata");
        assert_eq!(files_before, files_after, "must not insert prd_files");
    }
}
