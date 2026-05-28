//! Internal diagnostics logging (CONTRACT-LOG-001 channel B).
//!
//! Channel B is `tracing`: the debug/trace/info/warn/error stream that nobody
//! snapshot-tests and that is not a product surface. It is intentionally the
//! *opposite* of [`crate::output::ui`] (channels A / A2), which preserves exact
//! bytes for operators and tests. Events here ARE decorated with level +
//! timestamp and are filtered into two layers:
//!
//! - **Console layer** → stderr, default `WARN`+, overridable via the
//!   `TASK_MGR_LOG` env var (same directive syntax as `RUST_LOG`). Keeping the
//!   console floor at WARN+ means routine debug/info never pollutes normal
//!   output or libtest's capture (learning #3295: a tracing writer to
//!   `io::stderr()` bypasses libtest's thread-local capture, so a noisier floor
//!   would leak into unrelated test output).
//! - **File layer** → `<.task-mgr>/logs/task-mgr-<prefix>.<YYYY-MM-DD>.log`,
//!   always `DEBUG`+, daily-rolling. The base name is suffixed by the active
//!   PRD task prefix — the same convention `tasks/progress-<prefix>.txt` uses
//!   — so concurrent loops on different PRDs never share one log file. Falls
//!   back to `task-mgr.<YYYY-MM-DD>.log` when no prefix resolves. The `.log`
//!   extension lands at the end (via `filename_suffix`) so editors and
//!   log-rotation tools that filetype-match `*.log` still recognize rotated
//!   files.
//!
//! [`init`] is **idempotent** and **best-effort**: if the log directory or file
//! cannot be opened it degrades to console-only and never aborts the CLI. The
//! `tracing-appender` [`WorkerGuard`] is retained for the process lifetime in a
//! `OnceLock` — dropping it early would silently discard buffered file writes.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Env var controlling the console layer's filter (same syntax as `RUST_LOG`).
const LOG_ENV: &str = "TASK_MGR_LOG";

/// Console verbosity floor when `TASK_MGR_LOG` is unset, empty, or malformed.
const DEFAULT_CONSOLE_DIRECTIVE: &str = "warn";

/// Retains the file appender's flush guard for the lifetime of the process.
/// `None` means the file layer could not be created (console-only fallback).
/// Storing it in a `OnceLock` both keeps the guard alive — dropping it would
/// lose buffered writes — and makes [`init`] idempotent: a second call observes
/// a populated cell and returns early.
static LOG_GUARD: OnceLock<Option<WorkerGuard>> = OnceLock::new();

/// Initialize the global `tracing` subscriber. Idempotent and best-effort.
///
/// `db_dir` is the resolved `.task-mgr` directory; logs are written under
/// `db_dir/logs/`. `active_prefix` is the active PRD task prefix (resolved by
/// the caller via the same source `task-mgr current` uses); `None` falls back
/// to an unsuffixed `task-mgr.<YYYY-MM-DD>.log` base name.
///
/// Always returns `Ok(())` — a file-open failure degrades to console-only and
/// is never propagated. Call this once from `main` before any loop work.
pub fn init(db_dir: &Path, active_prefix: Option<&str>) -> crate::TaskMgrResult<()> {
    if LOG_GUARD.get().is_some() {
        return Ok(()); // already initialized — no-op
    }

    let (console_filter, malformed) = console_env_filter(std::env::var(LOG_ENV).ok().as_deref());

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
        .with_filter(console_filter);

    // Best-effort file writer; `None` → console-only. Build the layer only when
    // the writer exists (Option<Layer> is itself a no-op Layer when None).
    let (file_writer, guard) = match build_file_writer(&log_dir(db_dir), active_prefix) {
        Some((writer, guard)) => (Some(writer), Some(guard)),
        None => (None, None),
    };
    let file_layer = file_writer.map(|writer| {
        fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(LevelFilter::DEBUG)
    });

    // `try_init` returns Err only if a global default is already installed; the
    // `LOG_GUARD` short-circuit above makes that unreachable in normal use, so
    // we ignore it defensively rather than panicking.
    let _ = tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .try_init();

    // Store the guard (or `None`) so it lives for the process and marks init done.
    let _ = LOG_GUARD.set(guard);

    if malformed {
        crate::output::ui::emit_err(&format!(
            "task-mgr: ignoring malformed {LOG_ENV}; using {DEFAULT_CONSOLE_DIRECTIVE}+ console logging"
        ));
    }

    Ok(())
}

/// Directory holding rolling log files: `<.task-mgr>/logs/`.
fn log_dir(db_dir: &Path) -> PathBuf {
    db_dir.join("logs")
}

/// Stem of the log file name (no `.log` extension), suffixed by the active PRD
/// prefix exactly like `tasks/progress-<prefix>.txt`.
///
/// `None` (or blank) → bare `task-mgr` (no prefix segment). The rolling appender
/// adds the date between this stem and the `.log` extension via
/// [`LOG_FILE_EXTENSION`] / `filename_suffix`, producing on-disk names like
/// `task-mgr-abc12345.2026-05-27.log` — the conventional `*.log` form that
/// editors / log-rotation tools recognize.
fn log_file_base(active_prefix: Option<&str>) -> String {
    match active_prefix.map(str::trim).filter(|p| !p.is_empty()) {
        Some(prefix) => format!("task-mgr-{prefix}"),
        None => "task-mgr".to_string(),
    }
}

/// Extension used by the rolling file appender (`filename_suffix`). Kept as a
/// constant so the test that reads files back stays in sync with the appender.
const LOG_FILE_EXTENSION: &str = "log";

/// Build the console `EnvFilter` from an explicit env value (`None` = unset).
///
/// Returns `(filter, malformed)` where `malformed` is true iff a non-empty value
/// failed to parse — the caller emits a one-time note and we fall back to
/// [`DEFAULT_CONSOLE_DIRECTIVE`]. Unset/empty silently defaults (no note).
fn console_env_filter(raw: Option<&str>) -> (EnvFilter, bool) {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => match EnvFilter::try_new(value) {
            Ok(filter) => (filter, false),
            Err(_) => (EnvFilter::new(DEFAULT_CONSOLE_DIRECTIVE), true),
        },
        None => (EnvFilter::new(DEFAULT_CONSOLE_DIRECTIVE), false),
    }
}

/// Build a non-blocking, daily-rolling file writer under `logs_dir`.
///
/// Best-effort and **non-panicking**: returns `None` (→ console-only) if the
/// directory cannot be created or the appender cannot be built. Uses the
/// fallible `RollingFileAppender::builder().build()` rather than the
/// `rolling::daily()` convenience constructor precisely so a read-only
/// `.task-mgr` degrades instead of crashing the CLI.
///
/// Crucially, we only create `logs/` when the parent `.task-mgr/` directory
/// already exists. If the project is not yet initialized we return `None`
/// (console-only) rather than bootstrapping a stray `.task-mgr/` from a
/// read-only command running outside an initialized project.
fn build_file_writer(
    logs_dir: &Path,
    active_prefix: Option<&str>,
) -> Option<(NonBlocking, WorkerGuard)> {
    // Degrade to console-only when the .task-mgr parent dir doesn't exist yet.
    let parent = logs_dir.parent()?;
    if !parent.is_dir() {
        return None;
    }
    std::fs::create_dir_all(logs_dir).ok()?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(log_file_base(active_prefix))
        .filename_suffix(LOG_FILE_EXTENSION)
        .build(logs_dir)
        .ok()?;
    Some(tracing_appender::non_blocking(appender))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inspect an `EnvFilter`'s max enabled level without touching global state.
    fn max_level(filter: &EnvFilter) -> Option<LevelFilter> {
        use tracing_subscriber::layer::Filter;
        <EnvFilter as Filter<tracing_subscriber::Registry>>::max_level_hint(filter)
    }

    #[test]
    fn log_file_base_is_prefix_suffixed() {
        // The base (no `.log`) carries the prefix; `filename_suffix(LOG_FILE_EXTENSION)`
        // adds `.log` AFTER the rotation date for conventional `*.log` files.
        assert_eq!(log_file_base(Some("abc12345")), "task-mgr-abc12345");
        assert_eq!(log_file_base(Some("  abc12345  ")), "task-mgr-abc12345");
    }

    #[test]
    fn log_file_base_falls_back_to_unsuffixed() {
        assert_eq!(log_file_base(None), "task-mgr");
        assert_eq!(log_file_base(Some("")), "task-mgr");
        assert_eq!(log_file_base(Some("   ")), "task-mgr");
    }

    #[test]
    fn console_filter_defaults_to_warn_when_unset() {
        let (filter, malformed) = console_env_filter(None);
        assert!(!malformed);
        assert_eq!(max_level(&filter), Some(LevelFilter::WARN));
    }

    #[test]
    fn console_filter_honors_debug() {
        let (filter, malformed) = console_env_filter(Some("debug"));
        assert!(!malformed);
        assert_eq!(max_level(&filter), Some(LevelFilter::DEBUG));
    }

    #[test]
    fn console_filter_malformed_falls_back_to_warn_and_flags() {
        // A directive that fails to parse must not panic; it degrades to WARN+
        // and signals `malformed` so the caller can emit a one-time note.
        let (filter, malformed) = console_env_filter(Some("=not a directive="));
        assert!(malformed, "malformed directive should be flagged");
        assert_eq!(max_level(&filter), Some(LevelFilter::WARN));
    }

    #[test]
    fn build_file_writer_degrades_on_unwritable_path() {
        // A regular file standing in for a directory parent: create_dir_all
        // cannot create `logs/` underneath it, so the writer must degrade to
        // None rather than panic (this is the read-only `.task-mgr` case).
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let logs = blocker.join("logs");
        assert!(build_file_writer(&logs, Some("p")).is_none());
    }

    #[test]
    fn init_returns_ok_and_degrades_on_unwritable_dir() {
        // init() must never abort even when the log file cannot be opened.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        // db_dir under a regular file → logs/ uncreatable → console-only.
        assert!(init(&blocker, None).is_ok());
    }

    #[test]
    fn file_layer_writes_and_retained_guard_flushes() {
        // Failure-mode guard: holding the WorkerGuard for the logging window
        // means buffered events actually land in the file. Dropping the guard
        // flushes the non-blocking worker before we read the file back.
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        let (writer, guard) =
            build_file_writer(&logs, Some("abc12345")).expect("writer should build");

        let layer = fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(LevelFilter::DEBUG);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!("obs-readback-marker");
        });
        drop(guard); // flush the non-blocking writer

        let names: Vec<String> = std::fs::read_dir(&logs)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let log_file = names
            .iter()
            .find(|n| {
                n.starts_with("task-mgr-abc12345.")
                    && n.ends_with(&format!(".{LOG_FILE_EXTENSION}"))
            })
            .unwrap_or_else(|| {
                panic!("expected prefix-suffixed `task-mgr-abc12345.<date>.log`, got {names:?}")
            });

        // Timestamps in the line are non-deterministic (learning #3855), so we
        // assert on the marker substring rather than exact bytes.
        let content = std::fs::read_to_string(logs.join(log_file)).unwrap();
        assert!(
            content.contains("obs-readback-marker"),
            "retained-guard write missing from log file: {content:?}"
        );
    }
}
