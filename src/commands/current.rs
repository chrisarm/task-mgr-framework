//! `task-mgr current` — print the resolved active prefix, source, and target PRD path.
//!
//! This command makes the implicit context explicit: agents can run `task-mgr
//! current` at the start of a session to see which PRD is active, how it was
//! resolved, and which JSON file backs it, without needing to inspect env vars
//! or prd_metadata directly.
//!
//! Exit 0 for the no-flag probe (`context: None` when no active PRD / ≥2
//! prefixes). Returns `Err` (CLI non-zero) for unregistered / missing /
//! directory `--from-json`. Default `target=` is the CLI write path (remap,
//! then existence policy) — not the unconditional loop remap.

use std::path::Path;

use serde::Serialize;

use crate::TaskMgrResult;
use crate::commands::context::ResolvedContext;

/// Result of `task-mgr current`.
#[derive(Debug, Clone, Serialize)]
pub struct CurrentResult {
    /// `None` when no active PRD could be resolved.
    pub context: Option<ResolvedContext>,
}

/// Resolve and return the active PRD context.
///
/// No-flag probe: `Ok` with `context: None` when there is no active PRD
/// (empty DB or ≥2 prefixes without env/flag) — that is not an error.
/// `--from-json` that is unregistered, missing, or a directory returns `Err`.
///
/// Uses `open_and_migrate` so the command is safe to run before `loop init`
/// (just `init` is enough; the schema + migrations are applied automatically).
pub fn current(db_dir: &Path, from_json: Option<&Path>) -> TaskMgrResult<CurrentResult> {
    let conn = crate::db::open_and_migrate(db_dir)?;
    let source_root = crate::git::main_repo_root_at(db_dir)
        .or_else(|| db_dir.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| db_dir.to_path_buf());
    let worktree_root = std::env::current_dir()
        .ok()
        .filter(|cwd| crate::git::is_inside_worktree_at(cwd).unwrap_or(false))
        .unwrap_or_else(|| source_root.clone());
    let context = crate::commands::context::resolve_context_with_roots(
        &conn,
        from_json,
        "current",
        Some(&source_root),
        Some(&worktree_root),
    )?;
    Ok(CurrentResult { context })
}

/// Format for `--format text`.
pub fn format_text(result: &CurrentResult) -> String {
    match &result.context {
        None => "no active PRD; pass --from-json or set TASK_MGR_ACTIVE_PREFIX".to_string(),
        Some(ctx) => {
            let target = if ctx.prd_json_path.as_os_str().is_empty() {
                "(none)".to_string()
            } else {
                ctx.prd_json_path.display().to_string()
            };
            format!(
                "active prefix={}  source={}  target={}",
                ctx.prefix, ctx.source, target,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rusqlite::Connection;

    use super::*;
    use crate::commands::context::{ResolutionSource, resolve_context};
    use crate::db::migrations::run_migrations;
    use crate::db::schema::create_schema;

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

    const ACTIVE_PREFIX_ENV: &str = crate::loop_engine::claude::ACTIVE_PREFIX_ENV;

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
    /// before the mutex is released (learning #4883). Uses the crate-level
    /// `ENV_PREFIX_MUTEX` so current/add/context tests serialize together.
    struct EnvIsolation {
        _env: EnvVarGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl EnvIsolation {
        fn unset(name: &'static str) -> Self {
            let lock = crate::ENV_PREFIX_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let env = EnvVarGuard::unset(name);
            Self {
                _env: env,
                _lock: lock,
            }
        }
        fn set(name: &'static str, value: &str) -> Self {
            let lock = crate::ENV_PREFIX_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let env = EnvVarGuard::set(name, value);
            Self {
                _env: env,
                _lock: lock,
            }
        }
    }

    // Unit tests for ResolutionSource variants (acceptance criteria)

    #[test]
    fn test_resolution_source_env_var_when_env_set_and_matches() {
        let _iso = EnvIsolation::set(ACTIVE_PREFIX_ENV, "A");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");

        let ctx = resolve_context(&conn, None, "current")
            .unwrap()
            .expect("should resolve");
        assert_eq!(ctx.source, ResolutionSource::EnvVar);
        assert_eq!(ctx.prefix, "A");
    }

    #[test]
    fn test_resolution_source_single_prefix_when_env_empty_single_prd() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");

        let ctx = resolve_context(&conn, None, "current")
            .unwrap()
            .expect("should resolve");
        assert_eq!(ctx.source, ResolutionSource::SinglePrefix);
        assert_eq!(ctx.prefix, "A");
    }

    #[test]
    fn test_resolution_source_none_when_env_empty_zero_prefixes() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();

        let result = resolve_context(&conn, None, "current").unwrap();
        assert!(result.is_none(), "should return None for zero prefixes");
    }

    #[test]
    fn test_resolution_source_none_when_env_empty_two_prefixes() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");

        let result = resolve_context(&conn, None, "current").unwrap();
        assert!(
            result.is_none(),
            "should return None for 2+ prefixes without env pin"
        );
    }

    #[test]
    fn test_current_empty_db_exits_ok_with_guidance() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        let ctx = resolve_context(&conn, None, "current").unwrap();
        let result = CurrentResult { context: ctx };
        assert!(result.context.is_none());
        let text = format_text(&result);
        assert!(text.contains("no active PRD"), "{text}");
    }

    #[test]
    fn test_stale_pin_from_current_does_not_say_add() {
        let _iso = EnvIsolation::set(ACTIVE_PREFIX_ENV, "stale-");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        let err = resolve_context(&conn, None, "current").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("current"),
            "stale-pin must name current: {msg}"
        );
        assert!(
            !msg.contains("`add`") && !msg.contains("command add"),
            "stale-pin from current must not say add: {msg}"
        );
    }

    // Tests for format_text — no env var needed, pure formatting

    #[test]
    fn test_format_text_no_context_prints_guidance() {
        let result = CurrentResult { context: None };
        let text = format_text(&result);
        assert!(
            text.contains("no active PRD"),
            "must contain 'no active PRD': {text}"
        );
        assert!(
            text.contains("TASK_MGR_ACTIVE_PREFIX"),
            "must name the env var: {text}"
        );
    }

    #[test]
    fn test_format_text_with_context_prints_fields() {
        let result = CurrentResult {
            context: Some(ResolvedContext {
                prefix: "abc123".to_string(),
                source: ResolutionSource::EnvVar,
                prd_json_path: PathBuf::from("tasks/prd.json"),
            }),
        };
        let text = format_text(&result);
        assert!(
            text.starts_with("active prefix=abc123"),
            "wrong format: {text}"
        );
        assert!(text.contains("source=env"), "wrong source: {text}");
        assert!(
            text.contains("target=tasks/prd.json"),
            "wrong target: {text}"
        );
    }

    #[test]
    fn test_format_text_empty_path_shows_none() {
        let result = CurrentResult {
            context: Some(ResolvedContext {
                prefix: "P".to_string(),
                source: ResolutionSource::SinglePrefix,
                prd_json_path: PathBuf::new(),
            }),
        };
        let text = format_text(&result);
        assert!(
            text.contains("(none)"),
            "empty path should display as (none): {text}"
        );
    }

    #[test]
    fn test_from_json_registered_source_and_canonical_target() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let dir = tempfile::TempDir::new().unwrap();
        let prd = dir.path().join("pin.json");
        std::fs::write(
            &prd,
            r#"{"project":"p","taskPrefix":"PIN","userStories":[]}"#,
        )
        .unwrap();
        crate::commands::init::init(
            dir.path(),
            &[&prd],
            false,
            false,
            false,
            false,
            crate::commands::init::PrefixMode::Explicit("PIN".to_string()),
        )
        .unwrap();

        let result = current(dir.path(), Some(&prd)).unwrap();
        let ctx = result
            .context
            .as_ref()
            .expect("registered pin must resolve");
        assert_eq!(ctx.source, ResolutionSource::FromJsonFlag);
        assert_eq!(ctx.prefix, "PIN");
        assert_eq!(
            ctx.prd_json_path.canonicalize().unwrap(),
            prd.canonicalize().unwrap(),
            "target must be the canonical --from-json PATH"
        );
        let text = format_text(&result);
        assert!(text.contains("source=from-json"), "{text}");
    }

    #[test]
    fn test_from_json_unregistered_errors_naming_loop_init() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let dir = tempfile::TempDir::new().unwrap();
        // Empty schema so open_and_migrate works; no registered task_list.
        let _ = crate::db::open_and_migrate(dir.path()).unwrap();
        let orphan = dir.path().join("orphan.json");
        std::fs::write(
            &orphan,
            r#"{"project":"x","taskPrefix":"ORPHAN","userStories":[]}"#,
        )
        .unwrap();

        let err = current(dir.path(), Some(&orphan)).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("loop init"), "must name loop init: {msg}");
        assert!(
            msg.contains("not a registered task_list"),
            "unregistered copy: {msg}"
        );
    }

    #[test]
    fn test_from_json_missing_errors() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let dir = tempfile::TempDir::new().unwrap();
        let _ = crate::db::open_and_migrate(dir.path()).unwrap();
        let missing = dir.path().join("missing.json");
        let err = current(dir.path(), Some(&missing)).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("does not exist"), "{msg}");
    }

    #[test]
    fn test_from_json_directory_errors() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let dir = tempfile::TempDir::new().unwrap();
        let _ = crate::db::open_and_migrate(dir.path()).unwrap();
        let as_dir = dir.path().join("a-directory");
        std::fs::create_dir_all(&as_dir).unwrap();
        let err = current(dir.path(), Some(&as_dir)).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not a regular file") || msg.contains("directory"),
            "{msg}"
        );
    }

    #[test]
    fn test_two_prefixes_without_pin_is_ok_none_probe() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");
        // Mirror current()'s resolve call: ≥2 prefixes → Ok(None), not Err.
        let ctx = resolve_context(&conn, None, "current").unwrap();
        assert!(ctx.is_none());
        let text = format_text(&CurrentResult { context: ctx });
        assert!(text.contains("no active PRD"), "{text}");
    }
}
