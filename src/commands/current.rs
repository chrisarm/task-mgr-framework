//! `task-mgr current` — print the resolved active prefix, source, and target PRD path.
//!
//! This command makes the implicit context explicit: agents can run `task-mgr
//! current` at the start of a session to see which PRD is active, how it was
//! resolved, and which JSON file backs it, without needing to inspect env vars
//! or prd_metadata directly.

use std::path::Path;

use serde::Serialize;

use crate::TaskMgrResult;
use crate::commands::add::{ResolvedContext, resolve_context};

/// Result of `task-mgr current`.
#[derive(Debug, Clone, Serialize)]
pub struct CurrentResult {
    /// `None` when no active PRD could be resolved.
    pub context: Option<ResolvedContext>,
}

/// Resolve and return the active PRD context. Never returns an error for
/// "no active PRD" — that is surfaced as `context: None`.
///
/// Uses `open_and_migrate` so the command is safe to run before `init --from-json`
/// (just `init` is enough; the schema + migrations are applied automatically).
pub fn current(db_dir: &Path) -> TaskMgrResult<CurrentResult> {
    let conn = crate::db::open_and_migrate(db_dir)?;
    let context = resolve_context(&conn)?;
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
    use crate::commands::add::{ResolutionSource, resolve_context};
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

    /// Process-wide mutex: env var manipulation is not safe across threads.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvIsolation {
        _lock: std::sync::MutexGuard<'static, ()>,
        name: &'static str,
        prior: Option<String>,
    }
    impl EnvIsolation {
        fn unset(name: &'static str) -> Self {
            let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let prior = std::env::var(name).ok();
            unsafe { std::env::remove_var(name) };
            Self {
                _lock: lock,
                name,
                prior,
            }
        }
        fn set(name: &'static str, value: &str) -> Self {
            let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let prior = std::env::var(name).ok();
            unsafe { std::env::set_var(name, value) };
            Self {
                _lock: lock,
                name,
                prior,
            }
        }
    }
    impl Drop for EnvIsolation {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var(self.name, v) },
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }

    // Unit tests for ResolutionSource variants (acceptance criteria)

    #[test]
    fn test_resolution_source_env_var_when_env_set_and_matches() {
        let _iso = EnvIsolation::set(ACTIVE_PREFIX_ENV, "A");
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");

        let ctx = resolve_context(&conn).unwrap().expect("should resolve");
        assert_eq!(ctx.source, ResolutionSource::EnvVar);
        assert_eq!(ctx.prefix, "A");
    }

    #[test]
    fn test_resolution_source_single_prefix_when_env_empty_single_prd() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");

        let ctx = resolve_context(&conn).unwrap().expect("should resolve");
        assert_eq!(ctx.source, ResolutionSource::SinglePrefix);
        assert_eq!(ctx.prefix, "A");
    }

    #[test]
    fn test_resolution_source_none_when_env_empty_zero_prefixes() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();

        let result = resolve_context(&conn).unwrap();
        assert!(result.is_none(), "should return None for zero prefixes");
    }

    #[test]
    fn test_resolution_source_none_when_env_empty_two_prefixes() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        seed_prefix(&conn, 1, "alpha", "A");
        seed_prefix(&conn, 2, "beta", "B");

        let result = resolve_context(&conn).unwrap();
        assert!(
            result.is_none(),
            "should return None for 2+ prefixes without env pin"
        );
    }

    #[test]
    fn test_current_empty_db_exits_ok_with_guidance() {
        let _iso = EnvIsolation::unset(ACTIVE_PREFIX_ENV);
        let conn = memory_db();
        let ctx = resolve_context(&conn).unwrap();
        let result = CurrentResult { context: ctx };
        assert!(result.context.is_none());
        let text = format_text(&result);
        assert!(text.contains("no active PRD"), "{text}");
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
}
