/// Signal handling for the autonomous agent loop.
///
/// Supports two signal mechanisms:
/// 1. **File-based signals**: `.stop` and `.pause` files in the tasks directory
/// 2. **UNIX signals**: SIGINT (Ctrl+C) and SIGTERM via `Arc<AtomicBool>`
///
/// Session guidance accumulation lives in [`super::guidance`].
///
/// Run records under `{db_dir}/loop-runs/` and `task-mgr loop stop --prefix`
/// also live here (FEAT-003).
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::guidance::SessionGuidance;
use super::{DEADLINE_FILE_PREFIX, PAUSE_FILE, STOP_FILE};
use crate::db::prefix::validate_prefix;
use crate::output::ui;
use crate::{TaskMgrError, TaskMgrResult};

/// Check if a stop signal exists for the given session.
///
/// When `prefix` is `Some(p)`, checks `.stop-{p}` first (fast path), then falls back
/// to the global `.stop` file. When `prefix` is `None`, checks only `.stop`.
pub fn check_stop_signal(tasks_dir: &Path, prefix: Option<&str>) -> bool {
    if let Some(p) = prefix
        && tasks_dir.join(format!("{STOP_FILE}-{p}")).exists()
    {
        return true;
    }
    tasks_dir.join(STOP_FILE).exists()
}

/// Check if a pause signal exists for the given session.
///
/// When `prefix` is `Some(p)`, checks `.pause-{p}` first (fast path), then falls back
/// to the global `.pause` file. When `prefix` is `None`, checks only `.pause`.
pub fn check_pause_signal(tasks_dir: &Path, prefix: Option<&str>) -> bool {
    if let Some(p) = prefix
        && tasks_dir.join(format!("{PAUSE_FILE}-{p}")).exists()
    {
        return true;
    }
    tasks_dir.join(PAUSE_FILE).exists()
}

/// Clean up signal files for a specific session prefix.
///
/// When `prefix` is `Some(p)`: removes `.stop-{p}` and `.pause-{p}`, and also
/// removes the global `.stop`/`.pause` if present (since the engine's
/// `check_stop_signal` falls back to global files, they must be cleaned up
/// too — otherwise the stop signal persists across subsequent runs).
/// When `prefix` is `None`: removes global `.stop` and `.pause`.
pub fn cleanup_signal_files_for_prefix(tasks_dir: &Path, prefix: Option<&str>) {
    let mut files_to_remove = vec![tasks_dir.join(STOP_FILE), tasks_dir.join(PAUSE_FILE)];
    if let Some(p) = prefix {
        files_to_remove.push(tasks_dir.join(format!("{STOP_FILE}-{p}")));
        files_to_remove.push(tasks_dir.join(format!("{PAUSE_FILE}-{p}")));
    }
    for path in &files_to_remove {
        if path.exists()
            && let Err(e) = fs::remove_file(path)
        {
            tracing::warn!("could not remove {}: {}", path.display(), e);
        }
    }
}

/// Handle a pause signal: display banner, read multi-line stdin, accumulate guidance.
///
/// Reads lines from stdin until an empty line is entered. The collected text
/// is added to `session_guidance` with the current iteration tag. The pause
/// file that matched ([`pause_requested`] order: canonical prefix, canonical
/// global, then a fresh extra-dir prefix file) is deleted after the interaction
/// so the next iteration does not pause again.
///
/// Returns `true` if guidance was provided, `false` if user just resumed.
pub fn handle_pause(
    locations: &SignalLocations,
    iteration: u32,
    session_guidance: &mut SessionGuidance,
    prefix: Option<&str>,
) -> bool {
    ui::emit("\n╔══════════════════════════════════════════╗");
    ui::emit(&format!(
        "║          PAUSED (iteration {:<4})         ║",
        iteration
    ));
    ui::emit("╠══════════════════════════════════════════╣");
    ui::emit("║  Enter guidance (empty line to resume):  ║");
    ui::emit("╚══════════════════════════════════════════╝\n");

    let lines = read_lines_with_timeout(io::BufReader::new(io::stdin()), None);
    remove_matching_pause_file(locations, prefix);

    let text = lines.join("\n");
    let has_guidance = !text.trim().is_empty();

    if has_guidance {
        ui::emit("Guidance recorded. Resuming...\n");
        session_guidance.add(iteration, text);
    } else {
        ui::emit("Resuming without guidance...\n");
    }

    has_guidance
}

/// Best-effort unlink; warn on failure, never panic.
fn try_remove_signal_file(path: &Path) {
    if let Err(e) = fs::remove_file(path) {
        tracing::warn!("could not remove {}: {}", path.display(), e);
    }
}

/// Delete the pause file [`pause_requested`] would have matched (first hit).
fn remove_matching_pause_file(locations: &SignalLocations, prefix: Option<&str>) {
    if let Some(p) = prefix {
        let prefix_path = locations.canonical.join(format!("{PAUSE_FILE}-{p}"));
        if prefix_path.exists() {
            try_remove_signal_file(&prefix_path);
            return;
        }
    }
    let global = locations.canonical.join(PAUSE_FILE);
    if global.exists() {
        try_remove_signal_file(&global);
        return;
    }
    let Some(p) = prefix else {
        return;
    };
    let name = format!("{PAUSE_FILE}-{p}");
    for dir in &locations.extras {
        let path = dir.join(&name);
        if path.exists() && file_mtime_strictly_after(&path, locations.started_at) {
            try_remove_signal_file(&path);
            return;
        }
    }
}

/// Shared signal flag for SIGINT/SIGTERM detection.
///
/// Use `setup_signal_handler()` to install the async handler, then check
/// `is_signaled()` at iteration boundaries.
#[derive(Clone)]
pub struct SignalFlag {
    flag: Arc<AtomicBool>,
}

impl SignalFlag {
    /// Create a new signal flag (initially false).
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if a signal has been received.
    pub fn is_signaled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Set the signal flag (called by signal handler).
    pub fn set(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Get a clone of the inner Arc for use in async handlers.
    pub fn inner(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// Wrap a shared `Arc<AtomicBool>` as a `SignalFlag` so callers that need
    /// to pass `&SignalFlag` to an existing API can do so without owning the
    /// canonical flag. Used by `reconcile_stale_ephemeral_slots` (FEAT-005)
    /// when it builds a one-off `LlmMergeResolver` at startup from an
    /// `AutoRecoveryConfig` whose own `signal_flag` field is an `Arc<AtomicBool>`.
    pub(crate) fn from_arc(flag: Arc<AtomicBool>) -> Self {
        Self { flag }
    }
}

impl Default for SignalFlag {
    fn default() -> Self {
        Self::new()
    }
}

/// Clean up signal files and deadline files from the tasks directory.
///
/// Removes: `.stop`, `.pause`, and any `.deadline-*` files.
/// Errors are logged but don't propagate — cleanup should never crash the loop.
pub fn cleanup_signal_files(tasks_dir: &Path) {
    // Remove specific signal files
    for filename in &[STOP_FILE, PAUSE_FILE] {
        let path = tasks_dir.join(filename);
        if path.exists()
            && let Err(e) = fs::remove_file(&path)
        {
            tracing::warn!("could not remove {}: {}", path.display(), e);
        }
    }

    // Remove .deadline-* files
    cleanup_deadline_files(tasks_dir);
}

/// Remove `.deadline-*` files from the tasks directory.
fn cleanup_deadline_files(tasks_dir: &Path) {
    let entries = match fs::read_dir(tasks_dir) {
        Ok(entries) => entries,
        Err(_) => return, // Can't read dir, skip
    };

    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with(DEADLINE_FILE_PREFIX)
            && let Err(e) = fs::remove_file(entry.path())
        {
            tracing::warn!(
                "could not remove deadline file {}: {}",
                entry.path().display(),
                e
            );
        }
    }
}

/// Get the path where a stop file should be created.
///
/// When `prefix` is `Some(p)`, returns the session-specific `.stop-{p}` path.
/// When `prefix` is `None`, returns the global `.stop` path.
pub fn stop_file_path(tasks_dir: &Path, prefix: Option<&str>) -> PathBuf {
    match prefix {
        Some(p) => tasks_dir.join(format!("{STOP_FILE}-{p}")),
        None => tasks_dir.join(STOP_FILE),
    }
}

/// Get the path where a pause file should be created.
///
/// When `prefix` is `Some(p)`, returns the session-specific `.pause-{p}` path.
/// When `prefix` is `None`, returns the global `.pause` path.
pub fn pause_file_path(tasks_dir: &Path, prefix: Option<&str>) -> PathBuf {
    match prefix {
        Some(p) => tasks_dir.join(format!("{PAUSE_FILE}-{p}")),
        None => tasks_dir.join(PAUSE_FILE),
    }
}

/// Directories watched for operator stop/pause files.
///
/// `canonical` uses the existing single-dir rule with no mtime gate.
/// `extras` honor only prefix stop/pause (inner loop) or global stop (batch),
/// and only when the file mtime is strictly after `started_at`.
///
/// `started_at` is captured once by the caller (process entry); these helpers
/// never call [`SystemTime::now`].
#[derive(Debug, Clone)]
pub struct SignalLocations {
    pub canonical: PathBuf,
    pub extras: Vec<PathBuf>,
    pub started_at: SystemTime,
}

/// Read a file's modified time. Shared by freshness predicates and the stale
/// extra-dir stop sweep so the metadata path cannot drift.
fn read_file_mtime(path: &Path) -> std::io::Result<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified())
}

/// True when `mtime` is strictly after `started_at`. Equal timestamps are stale.
fn mtime_strictly_after(mtime: SystemTime, started_at: SystemTime) -> bool {
    mtime > started_at
}

/// True when `path`'s modified time is strictly after `started_at`.
///
/// Equal timestamps are stale. Missing or unreadable metadata is not a signal
/// (logged via tracing; never panics).
fn file_mtime_strictly_after(path: &Path, started_at: SystemTime) -> bool {
    match read_file_mtime(path) {
        Ok(mtime) => mtime_strictly_after(mtime, started_at),
        Err(e) => {
            tracing::warn!(
                "could not read mtime for {}: {}; treating as no signal",
                path.display(),
                e
            );
            false
        }
    }
}

/// Multi-directory stop predicate for an inner loop.
///
/// Canonical: [`check_stop_signal`] (prefix file, else global `.stop`) with no
/// mtime gate. Extras: only `.stop-<prefix>` whose mtime is strictly after
/// `locations.started_at`. Global files in extras and `prefix: None` extras are
/// ignored.
pub fn stop_requested(locations: &SignalLocations, prefix: Option<&str>) -> bool {
    if check_stop_signal(&locations.canonical, prefix) {
        return true;
    }
    let Some(p) = prefix else {
        return false;
    };
    let name = format!("{STOP_FILE}-{p}");
    for dir in &locations.extras {
        let path = dir.join(&name);
        if path.exists() && file_mtime_strictly_after(&path, locations.started_at) {
            return true;
        }
    }
    false
}

/// Multi-directory pause predicate for an inner loop.
///
/// Canonical: [`check_pause_signal`] (prefix file, else global `.pause`) with no
/// mtime gate. Extras: only `.pause-<prefix>` whose mtime is strictly after
/// `locations.started_at`. Global files in extras and `prefix: None` extras are
/// ignored.
pub fn pause_requested(locations: &SignalLocations, prefix: Option<&str>) -> bool {
    if check_pause_signal(&locations.canonical, prefix) {
        return true;
    }
    let Some(p) = prefix else {
        return false;
    };
    let name = format!("{PAUSE_FILE}-{p}");
    for dir in &locations.extras {
        let path = dir.join(&name);
        if path.exists() && file_mtime_strictly_after(&path, locations.started_at) {
            return true;
        }
    }
    false
}

/// Batch between-PRD stop predicate (global `.stop` only).
///
/// Canonical: global `.stop` exists (no mtime gate). Extras: global `.stop`
/// whose mtime is strictly after `locations.started_at`. Prefix stop files in
/// extras are ignored.
pub fn batch_stop_requested(locations: &SignalLocations) -> bool {
    if locations.canonical.join(STOP_FILE).exists() {
        return true;
    }
    for dir in &locations.extras {
        let path = dir.join(STOP_FILE);
        if path.exists() && file_mtime_strictly_after(&path, locations.started_at) {
            return true;
        }
    }
    false
}

/// Launch `tasks/` candidate: `cwd` when its final component is `tasks`, else
/// `cwd/tasks`. Does not create the directory.
pub fn launch_tasks_candidate(cwd: &Path) -> PathBuf {
    if cwd.file_name().is_some_and(|n| n == "tasks") {
        cwd.to_path_buf()
    } else {
        cwd.join("tasks")
    }
}

/// Build [`SignalLocations`] for a loop or batch between-PRD check.
///
/// `canonical` is kept as provided (no mtime gate). Extra candidates, after
/// canonicalize: launch tasks dir, `worktree/tasks` when `Some`, and
/// `git::main_repo_root_at(source_root)/tasks` when `Some`. A missing directory,
/// canonicalize error, or path equal to canonical is omitted — startup is never
/// aborted for those. Slot worktrees are not passed here.
///
/// Uses `std::env::current_dir()` for the launch candidate; tests should call
/// [`build_signal_locations_with_cwd`] with an explicit cwd.
pub fn build_signal_locations(
    canonical: PathBuf,
    source_root: &Path,
    actual_worktree_path: Option<&Path>,
    started_at: SystemTime,
) -> SignalLocations {
    let cwd = std::env::current_dir().unwrap_or_else(|_| source_root.to_path_buf());
    build_signal_locations_with_cwd(
        canonical,
        source_root,
        actual_worktree_path,
        started_at,
        &cwd,
    )
}

/// Canonicalize `raw` candidates, drop missing/errors, drop equals-canonical, dedupe.
fn collect_deduped_extra_tasks_dirs(raw: Vec<PathBuf>, canonical: &Path) -> Vec<PathBuf> {
    let canonical_key = fs::canonicalize(canonical).unwrap_or_else(|_| canonical.to_path_buf());
    let mut extras = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cand in raw {
        let Ok(canon) = fs::canonicalize(&cand) else {
            continue;
        };
        if canon == canonical_key {
            continue;
        }
        if seen.insert(canon.clone()) {
            extras.push(canon);
        }
    }
    extras
}

/// Same as [`build_signal_locations`] with an explicit launch cwd (testable).
pub fn build_signal_locations_with_cwd(
    canonical: PathBuf,
    source_root: &Path,
    actual_worktree_path: Option<&Path>,
    started_at: SystemTime,
    cwd: &Path,
) -> SignalLocations {
    let mut raw: Vec<PathBuf> = Vec::with_capacity(3);
    raw.push(launch_tasks_candidate(cwd));
    if let Some(wt) = actual_worktree_path {
        raw.push(wt.join("tasks"));
    }
    if let Some(main) = crate::git::main_repo_root_at(source_root) {
        raw.push(main.join("tasks"));
    }
    let extras = collect_deduped_extra_tasks_dirs(raw, &canonical);
    SignalLocations {
        canonical,
        extras,
        started_at,
    }
}

/// Operator-facing notice after attempting to delete one stale extra-dir stop.
fn emit_stale_stop_sweep_notice(
    path: &Path,
    prefix: &str,
    canonical: &Path,
    remove_err: Option<std::io::Error>,
) {
    let abs = path.display().to_string();
    let canonical_stop = canonical
        .join(format!("{STOP_FILE}-{prefix}"))
        .display()
        .to_string();
    let lead = if remove_err.is_some() {
        format!(
            "Stale stop file {abs} predates this process and would have been ignored \
(could not delete it)"
        )
    } else {
        format!(
            "Removed stale stop file {abs} because it predates this process and would have been ignored"
        )
    };
    ui::emit(&format!(
        "{lead}. To stop this run, use `task-mgr loop stop --prefix {prefix}` or create the file again at {abs} or at {canonical_stop}."
    ));
    if let Some(e) = remove_err {
        tracing::warn!("failed to delete stale stop {abs}: {e}");
    }
}

/// Delete one extra-dir `.stop-<prefix>` when mtime ≤ `started_at`, then notify.
///
/// Unreadable metadata leaves the file in place (distinct from the predicate
/// path, which treats unreadable as "no signal").
fn sweep_one_stale_extra_stop(path: &Path, started_at: SystemTime, prefix: &str, canonical: &Path) {
    if !path.exists() {
        return;
    }
    match read_file_mtime(path) {
        Err(e) => {
            tracing::warn!(
                "could not read mtime for stale-stop candidate {}: {}; leaving in place",
                path.display(),
                e
            );
            return;
        }
        Ok(mtime) if mtime_strictly_after(mtime, started_at) => return,
        Ok(_) => {}
    }
    let remove_err = fs::remove_file(path).err();
    emit_stale_stop_sweep_notice(path, prefix, canonical, remove_err);
}

/// At loop start: delete each extra-dir `.stop-<prefix>` whose mtime is before
/// or equal to `locations.started_at`. Emits a warning that names the absolute
/// path, says it predates this process and would have been ignored, and tells
/// the operator how to stop now. A failed delete still warns and does not abort.
///
/// Leaves canonical prefix files, global `.stop`, pause files, and other
/// prefixes alone. Does not call [`SystemTime::now`].
pub fn sweep_stale_extra_prefix_stops(locations: &SignalLocations, prefix: Option<&str>) {
    let Some(p) = prefix else {
        return;
    };
    let name = format!("{STOP_FILE}-{p}");
    for dir in &locations.extras {
        sweep_one_stale_extra_stop(
            &dir.join(&name),
            locations.started_at,
            p,
            &locations.canonical,
        );
    }
}

/// Exit cleanup for one extra directory: delete `.stop-<prefix>` and
/// `.pause-<prefix>` only. Leaves a global `.stop` / `.pause` that lives only
/// in that extra. Never call [`cleanup_signal_files_for_prefix`] on an extra
/// (that helper also removes globals).
///
/// When two same-prefix loops share an extra dir, the first to exit may remove
/// the shared prefix files; there is no refcount.
pub fn cleanup_extra_prefix_signals(extra_dir: &Path, prefix: Option<&str>) {
    let Some(p) = prefix else {
        return;
    };
    for name in [format!("{STOP_FILE}-{p}"), format!("{PAUSE_FILE}-{p}")] {
        let path = extra_dir.join(name);
        if path.exists()
            && let Err(e) = fs::remove_file(&path)
        {
            tracing::warn!("could not remove {}: {}", path.display(), e);
        }
    }
}

/// Emit absolute canonical + extra stop-watch directories as untruncated stderr
/// lines (after the session banner box).
pub fn emit_stop_watch_paths(locations: &SignalLocations) {
    ui::emit(&format!(
        "Stop watch (canonical): {}",
        locations.canonical.display()
    ));
    for extra in &locations.extras {
        ui::emit(&format!("Stop watch (extra): {}", extra.display()));
    }
}

// ---------------------------------------------------------------------------
// Run records + `task-mgr loop stop --prefix` (FEAT-003)
// ---------------------------------------------------------------------------

/// Directory holding live loop/batch run records: `{db_dir}/loop-runs/`.
pub fn loop_runs_dir(db_dir: &Path) -> PathBuf {
    db_dir.join("loop-runs")
}

/// Path for a prefix loop run record. Validates `prefix` before joining.
pub fn loop_run_record_path(db_dir: &Path, prefix: &str) -> Result<PathBuf, String> {
    validate_prefix(prefix)?;
    Ok(loop_runs_dir(db_dir).join(format!("{prefix}.json")))
}

/// Path for the batch run record (`batch.json`).
pub fn batch_run_record_path(db_dir: &Path) -> PathBuf {
    loop_runs_dir(db_dir).join("batch.json")
}

/// Kind of process that wrote a run record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunRecordKind {
    Loop,
    Batch,
}

/// Persisted snapshot of a live `loop run` / `batch run` for `loop stop`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub pid: u32,
    pub kind: RunRecordKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Seconds since UNIX epoch when the process captured `started_at`.
    pub started_at_unix_secs: u64,
    pub canonical_tasks_dir: PathBuf,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_checkout: Option<PathBuf>,
    #[serde(default)]
    pub extra_tasks_dirs: Vec<PathBuf>,
}

fn system_time_unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ensure_loop_runs_dir(db_dir: &Path) -> io::Result<PathBuf> {
    let dir = loop_runs_dir(db_dir);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write_run_record_file(path: &Path, record: &RunRecord) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(path, json)
}

/// Write `{db_dir}/loop-runs/<prefix>.json` for a live loop. Overwrites any
/// leftover record from an earlier failed start. Call before the stale
/// extra-dir sweep so `loop stop` can see this run.
pub fn write_loop_run_record(
    db_dir: &Path,
    prefix: &str,
    locations: &SignalLocations,
    cwd: &Path,
    worktree: Option<&Path>,
    main_checkout: Option<&Path>,
) -> io::Result<()> {
    validate_prefix(prefix).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let _ = ensure_loop_runs_dir(db_dir)?;
    let path = loop_runs_dir(db_dir).join(format!("{prefix}.json"));
    let record = RunRecord {
        pid: std::process::id(),
        kind: RunRecordKind::Loop,
        prefix: Some(prefix.to_string()),
        started_at_unix_secs: system_time_unix_secs(locations.started_at),
        canonical_tasks_dir: locations.canonical.clone(),
        cwd: cwd.to_path_buf(),
        worktree: worktree.map(Path::to_path_buf),
        main_checkout: main_checkout.map(Path::to_path_buf),
        extra_tasks_dirs: locations.extras.clone(),
    };
    write_run_record_file(&path, &record)
}

/// Write `{db_dir}/loop-runs/batch.json` at `run_batch` start.
pub fn write_batch_run_record(
    db_dir: &Path,
    locations: &SignalLocations,
    cwd: &Path,
    main_checkout: Option<&Path>,
) -> io::Result<()> {
    let _ = ensure_loop_runs_dir(db_dir)?;
    let path = batch_run_record_path(db_dir);
    let record = RunRecord {
        pid: std::process::id(),
        kind: RunRecordKind::Batch,
        prefix: None,
        started_at_unix_secs: system_time_unix_secs(locations.started_at),
        canonical_tasks_dir: locations.canonical.clone(),
        cwd: cwd.to_path_buf(),
        worktree: None,
        main_checkout: main_checkout.map(Path::to_path_buf),
        extra_tasks_dirs: locations.extras.clone(),
    };
    write_run_record_file(&path, &record)
}

/// Delete the prefix loop run record after orchestrator signal cleanup (step 21).
/// Missing file is fine (early Err residual, or already removed).
pub fn delete_loop_run_record(db_dir: &Path, prefix: Option<&str>) {
    let Some(p) = prefix else {
        return;
    };
    let Ok(path) = loop_run_record_path(db_dir, p) else {
        return;
    };
    if path.exists()
        && let Err(e) = fs::remove_file(&path)
    {
        tracing::warn!("could not remove loop run record {}: {}", path.display(), e);
    }
}

/// Delete `batch.json` when `run_batch` returns (every exit path).
pub fn delete_batch_run_record(db_dir: &Path) {
    let path = batch_run_record_path(db_dir);
    if path.exists()
        && let Err(e) = fs::remove_file(&path)
    {
        tracing::warn!(
            "could not remove batch run record {}: {}",
            path.display(),
            e
        );
    }
}

/// RAII guard that deletes `batch.json` when dropped.
pub struct BatchRunRecordGuard {
    db_dir: PathBuf,
}

impl BatchRunRecordGuard {
    /// Write the batch record and return a guard that deletes it on drop.
    pub fn write(
        db_dir: &Path,
        locations: &SignalLocations,
        cwd: &Path,
        main_checkout: Option<&Path>,
    ) -> io::Result<Self> {
        write_batch_run_record(db_dir, locations, cwd, main_checkout)?;
        Ok(Self {
            db_dir: db_dir.to_path_buf(),
        })
    }
}

impl Drop for BatchRunRecordGuard {
    fn drop(&mut self) {
        delete_batch_run_record(&self.db_dir);
    }
}

fn load_run_record(path: &Path) -> Option<RunRecord> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// True when some argv element's final path component is exactly `task-mgr`
/// and other elements are the separate tokens (`loop` AND `run`) OR
/// (`batch` AND `run`). Not a substring. A single `loop-run` token fails.
/// Flat `task-mgr loop <prd>` (no `run`) fails closed.
pub fn cmdline_qualifies_as_loop_or_batch_run(argv: &[OsString]) -> bool {
    let has_task_mgr = argv.iter().any(|arg| {
        Path::new(arg)
            .file_name()
            .is_some_and(|name| name == OsStr::new("task-mgr"))
    });
    if !has_task_mgr {
        return false;
    }
    let has = |tok: &str| argv.iter().any(|a| a.as_os_str() == OsStr::new(tok));
    (has("loop") && has("run")) || (has("batch") && has("run"))
}

/// Split `/proc/<pid>/cmdline` bytes on NUL into argv elements.
pub fn parse_proc_cmdline(raw: &[u8]) -> Vec<OsString> {
    raw.split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| OsString::from_vec(part.to_vec()))
        .collect()
}

/// Read `/proc/<pid>/cmdline` and parse into argv. `None` on I/O error.
pub fn read_proc_cmdline(pid: u32) -> Option<Vec<OsString>> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(parse_proc_cmdline(&raw))
}

/// `kill(pid, 0) == 0` — process exists and is visible to us.
pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 is a existence/permission probe; no signal delivered.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn record_is_live<FAlive, FCmd>(record: &RunRecord, is_alive: &FAlive, read_cmdline: &FCmd) -> bool
where
    FAlive: Fn(u32) -> bool,
    FCmd: Fn(u32) -> Option<Vec<OsString>>,
{
    if !is_alive(record.pid) {
        return false;
    }
    match read_cmdline(record.pid) {
        Some(argv) => cmdline_qualifies_as_loop_or_batch_run(&argv),
        None => false,
    }
}

fn emit_candidate_dirs(record: &RunRecord) {
    ui::emit_err(&format!(
        "Candidate stop directory (canonical): {}",
        record.canonical_tasks_dir.display()
    ));
    for extra in &record.extra_tasks_dirs {
        ui::emit_err(&format!(
            "Candidate stop directory (extra): {}",
            extra.display()
        ));
    }
}

fn stop_file_for_prefix_record(record: &RunRecord, prefix: &str) -> PathBuf {
    record
        .canonical_tasks_dir
        .join(format!("{STOP_FILE}-{prefix}"))
}

fn stop_file_for_batch_record(record: &RunRecord) -> PathBuf {
    record.canonical_tasks_dir.join(STOP_FILE)
}

/// Unlink a stop file written for a pid that died (or no longer qualifies)
/// between check and write. Canonical stops have no mtime gate, so leaving the
/// file would halt a future run.
fn dead_pid_after_stop_write(written: &Path, record: &RunRecord) -> TaskMgrError {
    if let Err(e) = fs::remove_file(written) {
        tracing::warn!(
            "could not unlink stop file after dead-pid recheck {}: {}",
            written.display(),
            e
        );
    }
    TaskMgrError::InvalidState {
        resource_type: "Loop run".to_string(),
        id: record.pid.to_string(),
        expected: "still-alive task-mgr loop/batch run".to_string(),
        actual: "process exited or cmdline no longer qualifies after stop write; \
                 removed the stop file this command created"
            .to_string(),
    }
}

/// Write the stop file, then re-check the pid. If the process is no longer a
/// trusted live target, unlink **only** `written` and return an error.
fn write_stop_and_recheck<FAlive, FCmd>(
    written: &Path,
    record: &RunRecord,
    is_alive: &FAlive,
    read_cmdline: &FCmd,
) -> TaskMgrResult<PathBuf>
where
    FAlive: Fn(u32) -> bool,
    FCmd: Fn(u32) -> Option<Vec<OsString>>,
{
    if let Some(parent) = written.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            TaskMgrError::io_error(parent.display().to_string(), "creating stop parent dir", e)
        })?;
    }
    fs::write(written, b"").map_err(|e| {
        TaskMgrError::io_error(written.display().to_string(), "writing stop file", e)
    })?;

    if record_is_live(record, is_alive, read_cmdline) {
        return Ok(written.to_path_buf());
    }
    Err(dead_pid_after_stop_write(written, record))
}

/// Operator `task-mgr loop stop --prefix <prefix>` (production `/proc` + kill 0).
///
/// Resolves records under `{db_dir}/loop-runs/`. A live prefix record writes
/// `.stop-<prefix>` in that record's canonical dir. With no live prefix record,
/// a live `batch.json` writes global `.stop` there. Never writes relative to
/// the caller's cwd when no record is live.
pub fn loop_stop(db_dir: &Path, prefix: &str) -> TaskMgrResult<PathBuf> {
    loop_stop_with(db_dir, prefix, pid_is_alive, read_proc_cmdline)
}

/// Emit status for one run-record path: missing, or present but not live.
fn emit_stale_or_missing_record(
    kind: &str,
    path: &Path,
    record: &Option<RunRecord>,
    not_live_phrase: &str,
) {
    if let Some(rec) = record {
        ui::emit_err(&format!(
            "{kind} record at {} exists but pid {} is not a live task-mgr {not_live_phrase} run.",
            path.display(),
            rec.pid
        ));
        emit_candidate_dirs(rec);
    } else {
        ui::emit_err(&format!(
            "No {} record at {}.",
            kind.to_ascii_lowercase(),
            path.display()
        ));
    }
}

/// Fail-closed diagnostics when no live prefix or batch record can be stopped.
fn emit_no_live_run_diagnostics(
    db_dir: &Path,
    prefix: &str,
    prefix_path: &Path,
    batch_path: &Path,
    prefix_record: &Option<RunRecord>,
    batch_record: &Option<RunRecord>,
) {
    ui::emit_err(&format!(
        "No live loop or batch run found for prefix '{prefix}' under {}.",
        loop_runs_dir(db_dir).display()
    ));
    emit_stale_or_missing_record("Prefix", prefix_path, prefix_record, "loop/batch");
    emit_stale_or_missing_record("Batch", batch_path, batch_record, "batch/loop");
}

/// If `record` is live, write its stop file and recheck; else `None`.
fn try_stop_from_record<FAlive, FCmd, FPath>(
    record: &Option<RunRecord>,
    make_stop_path: FPath,
    is_alive: &FAlive,
    read_cmdline: &FCmd,
) -> Option<TaskMgrResult<PathBuf>>
where
    FAlive: Fn(u32) -> bool,
    FCmd: Fn(u32) -> Option<Vec<OsString>>,
    FPath: FnOnce(&RunRecord) -> PathBuf,
{
    let Some(rec) = record else {
        return None;
    };
    if !record_is_live(rec, is_alive, read_cmdline) {
        return None;
    }
    let written = make_stop_path(rec);
    Some(write_stop_and_recheck(
        &written,
        rec,
        is_alive,
        read_cmdline,
    ))
}

/// After prefix validation: try live prefix record, then live batch record,
/// else fail closed with diagnostics.
///
/// Stays as one function (above the 30-line helper target) so the
/// prefix→batch→fail-closed order and deferred batch-record load remain
/// obvious; further splits would only add call indirection.
fn loop_stop_resolve_and_write<FAlive, FCmd>(
    db_dir: &Path,
    prefix: &str,
    is_alive: FAlive,
    read_cmdline: FCmd,
) -> TaskMgrResult<PathBuf>
where
    FAlive: Fn(u32) -> bool,
    FCmd: Fn(u32) -> Option<Vec<OsString>>,
{
    let prefix_path = loop_runs_dir(db_dir).join(format!("{prefix}.json"));
    let prefix_record = load_run_record(&prefix_path);
    if let Some(result) = try_stop_from_record(
        &prefix_record,
        |rec| stop_file_for_prefix_record(rec, prefix),
        &is_alive,
        &read_cmdline,
    ) {
        return result;
    }

    let batch_path = batch_run_record_path(db_dir);
    let batch_record = load_run_record(&batch_path);
    if let Some(result) = try_stop_from_record(
        &batch_record,
        stop_file_for_batch_record,
        &is_alive,
        &read_cmdline,
    ) {
        return result;
    }

    emit_no_live_run_diagnostics(
        db_dir,
        prefix,
        &prefix_path,
        &batch_path,
        &prefix_record,
        &batch_record,
    );
    Err(TaskMgrError::NotFound {
        resource_type: "Live loop/batch run".to_string(),
        id: prefix.to_string(),
    })
}

/// Testable `loop stop` with injectable liveness and cmdline readers.
pub fn loop_stop_with<FAlive, FCmd>(
    db_dir: &Path,
    prefix: &str,
    is_alive: FAlive,
    read_cmdline: FCmd,
) -> TaskMgrResult<PathBuf>
where
    FAlive: Fn(u32) -> bool,
    FCmd: Fn(u32) -> Option<Vec<OsString>>,
{
    // validate_prefix BEFORE any Path::join involving the prefix.
    if let Err(msg) = validate_prefix(prefix) {
        return Err(TaskMgrError::InvalidConfig {
            field: "prefix".to_string(),
            message: msg,
        });
    }
    loop_stop_resolve_and_write(db_dir, prefix, is_alive, read_cmdline)
}

/// Handle a human review checkpoint after a `requires_human` task completes.
///
/// Displays a banner with `task_id`, `task_title`, and optional `task_notes`,
/// then reads multi-line input from `reader` until an empty line or EOF.
/// Guidance is tagged as `[Human Review for {task_id}] {input}` and added to
/// `session_guidance`.
///
/// Returns `true` if guidance was provided, `false` if the user skipped or
/// input was EOF.
///
/// `timeout_secs: None` or `Some(0)` means a blocking read (no timeout).
/// `timeout_secs: Some(n)` where `n > 0` means return `false` after `n` seconds
/// without input.
///
/// # Panics
/// Never panics. EOF or I/O errors are treated as "no guidance provided".
pub fn handle_human_review<R: io::BufRead + Send + 'static>(
    reader: R,
    task_id: &str,
    task_title: &str,
    task_notes: Option<&str>,
    iteration: u32,
    session_guidance: &mut SessionGuidance,
    timeout_secs: Option<u32>,
) -> bool {
    let banner = format_human_review_banner(task_id, task_title, task_notes);
    // The banner already carries its own trailing newline and is immediately
    // followed by a blocking stdin read, so it goes through `ui::prompt`
    // (no appended newline) rather than `ui::emit`.
    ui::prompt(&banner);

    let lines = read_lines_with_timeout(reader, timeout_secs);
    let text = lines.join("\n");
    let has_guidance = !text.trim().is_empty();

    if has_guidance {
        let tagged = format!("[Human Review for {task_id}] {text}");
        ui::emit("Guidance recorded. Continuing...\n");
        session_guidance.add(iteration, tagged);
    } else {
        ui::emit("Skipping human review (no input).\n");
    }

    has_guidance
}

/// Format the human review banner string for display.
///
/// Returns a multi-line string containing the banner with task ID, title,
/// and notes (when present). The caller prints it to stderr.
pub fn format_human_review_banner(
    task_id: &str,
    task_title: &str,
    task_notes: Option<&str>,
) -> String {
    let sep = "═".repeat(44);
    let mut banner = format!("\n╔{sep}╗\n");
    banner.push_str("║           HUMAN REVIEW CHECKPOINT           ║\n");
    banner.push_str(&format!("╠{sep}╣\n"));
    banner.push_str(&format!("  Task:  {task_id}\n"));
    banner.push_str(&format!("  Title: {task_title}\n"));
    if let Some(notes) = task_notes {
        banner.push_str(&format!("  Notes: {notes}\n"));
    }
    banner.push_str(&format!("╠{sep}╣\n"));
    banner.push_str("  Enter feedback (empty line to skip):\n");
    banner.push_str(&format!("╚{sep}╝\n"));
    banner
}

/// Read lines from `reader` until an empty line or EOF, with optional timeout.
///
/// `timeout_secs: None` or `Some(0)` → blocking read.
/// `timeout_secs: Some(n > 0)` → spawn reader thread; collect lines until timeout fires.
pub(crate) fn read_lines_with_timeout<R: io::BufRead + Send + 'static>(
    reader: R,
    timeout_secs: Option<u32>,
) -> Vec<String> {
    match timeout_secs {
        None | Some(0) => {
            let mut lines = Vec::new();
            let mut saw_eof = true;
            for line_result in reader.lines() {
                saw_eof = false;
                match line_result {
                    Ok(line) if line.trim().is_empty() => break,
                    Ok(line) => lines.push(line),
                    Err(_) => break,
                }
            }
            if saw_eof && lines.is_empty() {
                ui::emit_err(
                    "Warning: EOF reached reading human review input. No guidance provided.",
                );
            }
            lines
        }
        Some(n) => {
            use std::sync::mpsc;
            use std::time::Duration;

            let (tx, rx) = mpsc::channel::<Option<String>>();
            std::thread::spawn(move || {
                for line_result in reader.lines() {
                    match line_result {
                        Ok(line) => {
                            let is_empty = line.trim().is_empty();
                            let _ = tx.send(Some(line));
                            if is_empty {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(None);
                            break;
                        }
                    }
                }
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(n));
            let mut lines = Vec::new();
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    if lines.is_empty() {
                        ui::emit("Human review timeout reached. No input provided.");
                    } else {
                        ui::emit("Human review timeout reached. Using partial input.");
                    }
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(Some(line)) if line.trim().is_empty() => break,
                    Ok(Some(line)) => lines.push(line),
                    Ok(None) => {
                        ui::emit_err(
                            "Warning: EOF reached reading human review input. No guidance provided.",
                        );
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if lines.is_empty() {
                            ui::emit("Human review timeout reached. No input provided.");
                        } else {
                            ui::emit("Human review timeout reached. Using partial input.");
                        }
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};
    use std::time::Duration;
    use tempfile::TempDir;

    // --- File signal tests ---

    #[test]
    fn test_check_stop_signal_returns_false_when_no_file() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_returns_true_when_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        assert!(check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_returns_true_with_content() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "reason: done for now").unwrap();
        assert!(check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_returns_false_when_no_file() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!check_pause_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_returns_true_when_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();
        assert!(check_pause_signal(temp_dir.path(), None));
    }

    // --- SignalFlag tests ---

    #[test]
    fn test_signal_flag_initially_false() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_set() {
        let flag = SignalFlag::new();
        flag.set();
        assert!(flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_clone_shares_state() {
        let flag1 = SignalFlag::new();
        let flag2 = flag1.clone();

        flag1.set();
        assert!(
            flag2.is_signaled(),
            "Cloned flag should see set from original"
        );
    }

    #[test]
    fn test_signal_flag_idempotent() {
        let flag = SignalFlag::new();
        flag.set();
        flag.set();
        flag.set();
        assert!(flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_inner_arc() {
        let flag = SignalFlag::new();
        let inner = flag.inner();
        inner.store(true, Ordering::Relaxed);
        assert!(flag.is_signaled());
    }

    #[test]
    fn test_signal_flag_default() {
        let flag = SignalFlag::default();
        assert!(!flag.is_signaled());
    }

    // --- Cleanup tests ---

    #[test]
    fn test_cleanup_signal_files_removes_stop_and_pause() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        cleanup_signal_files(temp_dir.path());

        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(!temp_dir.path().join(PAUSE_FILE).exists());
    }

    #[test]
    fn test_cleanup_signal_files_removes_deadline_files() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(".deadline-123"),
            "2024-01-01T00:00:00Z",
        )
        .unwrap();
        fs::write(
            temp_dir.path().join(".deadline-456"),
            "2024-01-01T12:00:00Z",
        )
        .unwrap();

        cleanup_signal_files(temp_dir.path());

        assert!(!temp_dir.path().join(".deadline-123").exists());
        assert!(!temp_dir.path().join(".deadline-456").exists());
    }

    #[test]
    fn test_cleanup_preserves_non_signal_files() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join("progress.txt"), "some progress").unwrap();
        fs::write(temp_dir.path().join("tasks.json"), "{}").unwrap();

        cleanup_signal_files(temp_dir.path());

        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(temp_dir.path().join("progress.txt").exists());
        assert!(temp_dir.path().join("tasks.json").exists());
    }

    #[test]
    fn test_cleanup_handles_nonexistent_files_gracefully() {
        let temp_dir = TempDir::new().unwrap();
        // No signal files exist — should not error
        cleanup_signal_files(temp_dir.path());
    }

    #[test]
    fn test_cleanup_handles_nonexistent_directory_gracefully() {
        let path = Path::new("/nonexistent/directory/path");
        // Should not panic
        cleanup_signal_files(path);
    }

    // --- Path helper tests ---

    #[test]
    fn test_stop_file_path() {
        let path = stop_file_path(Path::new("/project/tasks"), None);
        assert_eq!(path, PathBuf::from("/project/tasks/.stop"));
    }

    #[test]
    fn test_stop_file_path_with_prefix() {
        let path = stop_file_path(Path::new("/project/tasks"), Some("P1"));
        assert_eq!(path, PathBuf::from("/project/tasks/.stop-P1"));
    }

    #[test]
    fn test_pause_file_path() {
        let path = pause_file_path(Path::new("/project/tasks"), None);
        assert_eq!(path, PathBuf::from("/project/tasks/.pause"));
    }

    #[test]
    fn test_pause_file_path_with_prefix() {
        let path = pause_file_path(Path::new("/project/tasks"), Some("P1"));
        assert_eq!(path, PathBuf::from("/project/tasks/.pause-P1"));
    }

    // --- Per-session (prefix-scoped) signal file tests ---
    //
    // These tests define the expected behavior after prefix support is added to
    // check_stop_signal and check_pause_signal. They will fail to compile until
    // the functions accept `prefix: Option<&str>` as a second parameter.

    #[test]
    fn test_check_stop_signal_prefix_matches_session_specific_file() {
        // .stop-P1 exists → prefix "P1" triggers stop
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), Some("P1")));
    }

    #[test]
    fn test_check_stop_signal_prefix_no_match_other_session_file() {
        // .stop-P1 exists → prefix "P2" must NOT trigger stop (known-bad discriminator)
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        assert!(!check_stop_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_stop_signal_global_fallback_triggers_for_prefixed_session() {
        // Global .stop exists → any prefixed session (P1, P2) must trigger stop
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), Some("P1")));
        assert!(check_stop_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_stop_signal_global_fallback_triggers_for_no_prefix() {
        // Global .stop exists → session with no prefix must also trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_no_file_no_trigger_with_prefix() {
        // No signal files at all → must not trigger for any prefix
        let temp_dir = TempDir::new().unwrap();

        assert!(!check_stop_signal(temp_dir.path(), Some("P1")));
        assert!(!check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_session_specific_does_not_trigger_for_none_prefix() {
        // .stop-P1 exists but no global .stop → session with no prefix must NOT trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        assert!(!check_stop_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_stop_signal_prefix_file_takes_priority_over_global() {
        // Both .stop-P1 and global .stop exist → P1 prefix still triggers (via session file)
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();

        assert!(check_stop_signal(temp_dir.path(), Some("P1")));
    }

    #[test]
    fn test_check_pause_signal_prefix_matches_session_specific_file() {
        // .pause-P1 exists → prefix "P1" triggers pause
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();

        assert!(check_pause_signal(temp_dir.path(), Some("P1")));
    }

    #[test]
    fn test_check_pause_signal_prefix_no_match_other_session_file() {
        // .pause-P1 exists → prefix "P2" must NOT trigger pause
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();

        assert!(!check_pause_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_pause_signal_global_fallback_triggers_for_prefixed_session() {
        // Global .pause exists → any prefixed session (P1, P2) must trigger pause
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        assert!(check_pause_signal(temp_dir.path(), Some("P1")));
        assert!(check_pause_signal(temp_dir.path(), Some("P2")));
    }

    #[test]
    fn test_check_pause_signal_global_fallback_triggers_for_no_prefix() {
        // Global .pause exists → session with no prefix must also trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        assert!(check_pause_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_no_file_no_trigger() {
        // No signal files → must not trigger for any prefix
        let temp_dir = TempDir::new().unwrap();

        assert!(!check_pause_signal(temp_dir.path(), Some("P1")));
        assert!(!check_pause_signal(temp_dir.path(), None));
    }

    #[test]
    fn test_check_pause_signal_session_specific_does_not_trigger_for_none_prefix() {
        // .pause-P1 exists but no global .pause → session with no prefix must NOT trigger
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();

        assert!(!check_pause_signal(temp_dir.path(), None));
    }

    // --- Prefix-scoped cleanup tests ---

    #[test]
    fn test_cleanup_signal_files_prefix_removes_session_and_global_files() {
        // cleanup with prefix "P1" removes .stop-P1, .pause-P1, AND global .stop/.pause
        // (because check_stop_signal falls back to global, so both must be cleaned)
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();
        fs::write(temp_dir.path().join(".pause-P1"), "").unwrap();
        fs::write(temp_dir.path().join(".stop-P2"), "").unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();

        cleanup_signal_files_for_prefix(temp_dir.path(), Some("P1"));

        // Session-specific P1 files removed
        assert!(!temp_dir.path().join(".stop-P1").exists());
        assert!(!temp_dir.path().join(".pause-P1").exists());
        // Global files also removed (engine falls back to global)
        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(!temp_dir.path().join(PAUSE_FILE).exists());
        // Other session files preserved
        assert!(temp_dir.path().join(".stop-P2").exists());
    }

    #[test]
    fn test_cleanup_signal_files_no_prefix_removes_global_files_only() {
        // cleanup with no prefix removes global .stop and .pause, not session-specific
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        fs::write(temp_dir.path().join(PAUSE_FILE), "").unwrap();
        fs::write(temp_dir.path().join(".stop-P1"), "").unwrap();

        cleanup_signal_files_for_prefix(temp_dir.path(), None);

        assert!(!temp_dir.path().join(STOP_FILE).exists());
        assert!(!temp_dir.path().join(PAUSE_FILE).exists());
        // Session-specific file preserved
        assert!(temp_dir.path().join(".stop-P1").exists());
    }

    #[test]
    fn test_cleanup_signal_files_prefix_handles_nonexistent_files_gracefully() {
        // cleanup with a prefix when no matching files exist must not panic
        let temp_dir = TempDir::new().unwrap();
        cleanup_signal_files_for_prefix(temp_dir.path(), Some("P1"));
    }

    // --- SignalLocations multi-dir predicates (FEAT-001) ---

    fn epoch_plus(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn write_with_mtime(path: &Path, mtime: SystemTime) {
        fs::write(path, b"").unwrap();
        let file = File::options().write(true).open(path).unwrap();
        file.set_times(FileTimes::new().set_modified(mtime))
            .unwrap();
    }

    fn locations(canonical: &Path, extras: &[&Path], started_at: SystemTime) -> SignalLocations {
        SignalLocations {
            canonical: canonical.to_path_buf(),
            extras: extras.iter().map(|p| p.to_path_buf()).collect(),
            started_at,
        }
    }

    #[test]
    fn stop_requested_canonical_prefix_or_global_ignores_mtime() {
        let canonical = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        // Stale mtime (before started_at) still counts in canonical.
        write_with_mtime(&canonical.path().join(".stop-P1"), epoch_plus(500));
        let locs = locations(canonical.path(), &[], started);
        assert!(stop_requested(&locs, Some("P1")));

        let canonical2 = TempDir::new().unwrap();
        write_with_mtime(&canonical2.path().join(STOP_FILE), epoch_plus(100));
        let locs2 = locations(canonical2.path(), &[], started);
        assert!(stop_requested(&locs2, Some("P1")));
        assert!(stop_requested(&locs2, None));
    }

    #[test]
    fn pause_requested_canonical_prefix_or_global_ignores_mtime() {
        let canonical = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        write_with_mtime(&canonical.path().join(".pause-P1"), epoch_plus(500));
        let locs = locations(canonical.path(), &[], started);
        assert!(pause_requested(&locs, Some("P1")));

        let canonical2 = TempDir::new().unwrap();
        write_with_mtime(&canonical2.path().join(PAUSE_FILE), epoch_plus(100));
        let locs2 = locations(canonical2.path(), &[], started);
        assert!(pause_requested(&locs2, Some("P1")));
        assert!(pause_requested(&locs2, None));
    }

    #[test]
    fn stop_and_pause_requested_extra_prefix_strictly_after_started_at() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        write_with_mtime(&extra.path().join(".stop-P1"), epoch_plus(1_001));
        write_with_mtime(&extra.path().join(".pause-P1"), epoch_plus(1_002));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert!(stop_requested(&locs, Some("P1")));
        assert!(pause_requested(&locs, Some("P1")));
    }

    #[test]
    fn extra_prefix_mtime_before_or_equal_started_at_is_not_signal() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);

        // Before started_at
        write_with_mtime(&extra.path().join(".stop-P1"), epoch_plus(999));
        write_with_mtime(&extra.path().join(".pause-P1"), epoch_plus(900));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert!(!stop_requested(&locs, Some("P1")));
        assert!(!pause_requested(&locs, Some("P1")));

        // Equal to started_at (known-bad for >= ): must stay false for both
        write_with_mtime(&extra.path().join(".stop-P1"), started);
        write_with_mtime(&extra.path().join(".pause-P1"), started);
        let locs_eq = locations(canonical.path(), &[extra.path()], started);
        assert!(
            !stop_requested(&locs_eq, Some("P1")),
            "mtime == started_at must not stop"
        );
        assert!(
            !pause_requested(&locs_eq, Some("P1")),
            "mtime == started_at must not pause"
        );
    }

    #[test]
    fn other_prefix_in_canonical_or_extra_does_not_stop_p() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        write_with_mtime(&canonical.path().join(".stop-OTHER"), epoch_plus(2_000));
        write_with_mtime(&extra.path().join(".stop-OTHER"), epoch_plus(2_000));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert!(!stop_requested(&locs, Some("P1")));
    }

    #[test]
    fn global_stop_or_pause_in_extra_ignored_by_inner_predicates() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        write_with_mtime(&extra.path().join(STOP_FILE), epoch_plus(2_000));
        write_with_mtime(&extra.path().join(PAUSE_FILE), epoch_plus(2_000));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert!(!stop_requested(&locs, Some("P1")));
        assert!(!stop_requested(&locs, None));
        assert!(!pause_requested(&locs, Some("P1")));
        assert!(!pause_requested(&locs, None));
    }

    #[test]
    fn batch_stop_requested_canonical_global_no_mtime_extra_strict_after() {
        let started = epoch_plus(1_000);

        // Canonical global .stop with stale mtime still stops.
        let canonical = TempDir::new().unwrap();
        write_with_mtime(&canonical.path().join(STOP_FILE), epoch_plus(100));
        let locs = locations(canonical.path(), &[], started);
        assert!(batch_stop_requested(&locs));

        // Extra global .stop only when mtime > started_at.
        let canonical2 = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        write_with_mtime(&extra.path().join(STOP_FILE), epoch_plus(1_001));
        let locs_fresh = locations(canonical2.path(), &[extra.path()], started);
        assert!(batch_stop_requested(&locs_fresh));

        write_with_mtime(&extra.path().join(STOP_FILE), started);
        let locs_eq = locations(canonical2.path(), &[extra.path()], started);
        assert!(!batch_stop_requested(&locs_eq));

        write_with_mtime(&extra.path().join(STOP_FILE), epoch_plus(999));
        let locs_stale = locations(canonical2.path(), &[extra.path()], started);
        assert!(!batch_stop_requested(&locs_stale));
    }

    #[test]
    fn batch_stop_requested_ignores_prefix_files_in_extras() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        write_with_mtime(&extra.path().join(".stop-P1"), epoch_plus(2_000));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert!(!batch_stop_requested(&locs));
    }

    #[test]
    fn stop_and_batch_predicates_use_caller_started_at_not_clock() {
        // Fixed started_at; two calls agree. Helpers must not sample SystemTime::now().
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        write_with_mtime(&extra.path().join(".stop-P1"), epoch_plus(1_500));
        write_with_mtime(&extra.path().join(STOP_FILE), epoch_plus(1_500));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert_eq!(
            stop_requested(&locs, Some("P1")),
            stop_requested(&locs, Some("P1"))
        );
        assert_eq!(batch_stop_requested(&locs), batch_stop_requested(&locs));
        // A later started_at makes the same files stale.
        let locs_later = locations(canonical.path(), &[extra.path()], epoch_plus(2_000));
        assert!(!stop_requested(&locs_later, Some("P1")));
        assert!(!batch_stop_requested(&locs_later));
    }

    #[test]
    fn unreadable_extra_path_is_not_a_signal() {
        let canonical = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        // Point extras at a path that is not a directory — join + exists is false.
        let missing = canonical.path().join("does-not-exist");
        let locs = locations(canonical.path(), &[&missing], started);
        assert!(!stop_requested(&locs, Some("P1")));
        assert!(!pause_requested(&locs, Some("P1")));
        assert!(!batch_stop_requested(&locs));
    }

    // --- build / sweep / extra cleanup (FEAT-002) ---

    #[test]
    fn launch_tasks_candidate_uses_cwd_when_named_tasks() {
        let tmp = TempDir::new().unwrap();
        let tasks = tmp.path().join("tasks");
        fs::create_dir_all(&tasks).unwrap();
        assert_eq!(launch_tasks_candidate(&tasks), tasks);
        assert_eq!(launch_tasks_candidate(tmp.path()), tmp.path().join("tasks"));
    }

    #[test]
    fn build_signal_locations_drops_missing_equal_canonical_and_dedups() {
        let root = TempDir::new().unwrap();
        let canonical = root.path().join("canonical_tasks");
        fs::create_dir_all(&canonical).unwrap();
        let launch = root.path().join("launch").join("tasks");
        fs::create_dir_all(&launch).unwrap();
        let missing_parent = root.path().join("no_such");
        // worktree candidate exists
        let wt = root.path().join("wt");
        let wt_tasks = wt.join("tasks");
        fs::create_dir_all(&wt_tasks).unwrap();

        let started = epoch_plus(1_000);
        // cwd = launch parent so launch candidate is launch/tasks
        let locs = build_signal_locations_with_cwd(
            canonical.clone(),
            missing_parent.as_path(), // main_repo_root_at → None
            Some(wt.as_path()),
            started,
            launch.parent().unwrap(),
        );
        assert_eq!(locs.canonical, canonical);
        assert_eq!(locs.started_at, started);
        let canon_launch = fs::canonicalize(&launch).unwrap();
        let canon_wt = fs::canonicalize(&wt_tasks).unwrap();
        assert!(locs.extras.contains(&canon_launch));
        assert!(locs.extras.contains(&canon_wt));
        assert_eq!(locs.extras.len(), 2);

        // When launch equals canonical, it is dropped from extras.
        let locs_eq = build_signal_locations_with_cwd(
            launch.clone(),
            missing_parent.as_path(),
            None,
            started,
            launch.parent().unwrap(),
        );
        assert!(
            locs_eq.extras.is_empty(),
            "canonical twin must stay out of extras: {:?}",
            locs_eq.extras
        );
    }

    #[test]
    fn build_signal_locations_omits_canonicalize_error_without_abort() {
        let root = TempDir::new().unwrap();
        let canonical = root.path().join("c");
        fs::create_dir_all(&canonical).unwrap();
        let started = epoch_plus(500);
        // cwd points at a path whose tasks/ child does not exist → omitted.
        let locs = build_signal_locations_with_cwd(
            canonical.clone(),
            root.path().join("not-a-git-repo").as_path(),
            Some(root.path().join("missing-wt").as_path()),
            started,
            root.path().join("also-missing").as_path(),
        );
        assert_eq!(locs.canonical, canonical);
        assert!(locs.extras.is_empty());
    }

    #[test]
    fn known_bad_prd_parent_a_worktree_parent_b_keeps_a_as_canonical() {
        // Building canonical from the post-remap worktree PRD parent would drop
        // the mtime gate on a directory the loop does not watch as canonical.
        let root = TempDir::new().unwrap();
        let prd_parent_a = root.path().join("source").join("tasks");
        let worktree_b = root.path().join("worktree");
        let worktree_tasks_b = worktree_b.join("tasks");
        fs::create_dir_all(&prd_parent_a).unwrap();
        fs::create_dir_all(&worktree_tasks_b).unwrap();
        let started = epoch_plus(42);
        let locs = build_signal_locations_with_cwd(
            prd_parent_a.clone(),
            root.path().join("not-git").as_path(),
            Some(worktree_b.as_path()),
            started,
            root.path().join("elsewhere").as_path(), // no launch tasks/
        );
        assert_eq!(
            locs.canonical, prd_parent_a,
            "canonical must stay resolve_paths tasks_dir (A), not worktree PRD parent (B)"
        );
        let canon_b = fs::canonicalize(&worktree_tasks_b).unwrap();
        assert!(
            locs.extras.contains(&canon_b),
            "worktree tasks (B) must be an mtime-gated extra, got {:?}",
            locs.extras
        );
    }

    #[test]
    fn sweep_stale_extra_prefix_stops_deletes_stale_only() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        let stale = extra.path().join(".stop-P1");
        let fresh = extra.path().join(".stop-OTHER");
        let global = extra.path().join(STOP_FILE);
        let pause = extra.path().join(".pause-P1");
        let canon_stop = canonical.path().join(".stop-P1");
        write_with_mtime(&stale, epoch_plus(500));
        write_with_mtime(&fresh, epoch_plus(500));
        write_with_mtime(&global, epoch_plus(500));
        write_with_mtime(&pause, epoch_plus(500));
        write_with_mtime(&canon_stop, epoch_plus(500));

        let locs = locations(canonical.path(), &[extra.path()], started);
        sweep_stale_extra_prefix_stops(&locs, Some("P1"));

        assert!(!stale.exists(), "stale extra .stop-P1 must be deleted");
        assert!(fresh.exists(), "other prefix must remain");
        assert!(global.exists(), "extra global .stop must remain");
        assert!(pause.exists(), "pause must remain");
        assert!(canon_stop.exists(), "canonical .stop-P1 must remain");
    }

    #[test]
    fn sweep_leaves_fresh_extra_prefix_stop() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        let fresh = extra.path().join(".stop-P1");
        write_with_mtime(&fresh, epoch_plus(1_001));
        let locs = locations(canonical.path(), &[extra.path()], started);
        sweep_stale_extra_prefix_stops(&locs, Some("P1"));
        assert!(fresh.exists());
    }

    #[test]
    fn cleanup_extra_prefix_signals_removes_prefix_only() {
        let extra = TempDir::new().unwrap();
        fs::write(extra.path().join(".stop-P1"), b"").unwrap();
        fs::write(extra.path().join(".pause-P1"), b"").unwrap();
        fs::write(extra.path().join(STOP_FILE), b"").unwrap();
        fs::write(extra.path().join(PAUSE_FILE), b"").unwrap();
        fs::write(extra.path().join(".stop-OTHER"), b"").unwrap();

        cleanup_extra_prefix_signals(extra.path(), Some("P1"));

        assert!(!extra.path().join(".stop-P1").exists());
        assert!(!extra.path().join(".pause-P1").exists());
        assert!(extra.path().join(STOP_FILE).exists());
        assert!(extra.path().join(PAUSE_FILE).exists());
        assert!(extra.path().join(".stop-OTHER").exists());
    }

    #[test]
    fn remove_matching_pause_file_clears_extra_prefix_pause() {
        let canonical = TempDir::new().unwrap();
        let extra = TempDir::new().unwrap();
        let started = epoch_plus(1_000);
        let pause = extra.path().join(".pause-P1");
        write_with_mtime(&pause, epoch_plus(1_500));
        let locs = locations(canonical.path(), &[extra.path()], started);
        assert!(pause_requested(&locs, Some("P1")));
        remove_matching_pause_file(&locs, Some("P1"));
        assert!(!pause.exists());
        assert!(!pause_requested(&locs, Some("P1")));
    }

    // --- handle_human_review tests (require FEAT-004) ---

    #[test]
    fn test_handle_human_review_with_input_returns_true_and_records_guidance() {
        // Non-empty line followed by empty line terminates the read.
        let input = "my feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(
            cursor,
            "TASK-123",
            "Some task title",
            None,
            1,
            &mut guidance,
            None,
        );

        assert!(result, "Should return true when guidance was provided");
        assert!(!guidance.is_empty(), "Guidance should be recorded");
    }

    #[test]
    fn test_handle_human_review_with_empty_input_returns_false_no_guidance() {
        // Just pressing Enter (empty line) — no guidance provided.
        let input = "\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(
            cursor,
            "TASK-123",
            "Some task title",
            None,
            1,
            &mut guidance,
            None,
        );

        assert!(!result, "Should return false when only empty input given");
        assert!(
            guidance.is_empty(),
            "Guidance must not be recorded for empty input"
        );
    }

    #[test]
    fn test_handle_human_review_guidance_tagged_with_task_id() {
        // Guidance must be stored as "[Human Review for TASK-ID] {input}".
        let input = "review feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        handle_human_review(
            cursor,
            "FEAT-007",
            "Some feature",
            None,
            5,
            &mut guidance,
            None,
        );

        let formatted = guidance.format_for_prompt();
        assert!(
            formatted.contains("[Human Review for FEAT-007]"),
            "Guidance must be tagged with task ID; got: '{formatted}'"
        );
        assert!(
            formatted.contains("review feedback"),
            "Guidance must contain the input text; got: '{formatted}'"
        );
    }

    #[test]
    fn test_handle_human_review_stdin_eof_returns_false_no_panic() {
        // Empty reader simulates headless/piped stdin hitting EOF immediately.
        let input = "";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(
            cursor,
            "TASK-123",
            "Some task title",
            None,
            1,
            &mut guidance,
            None,
        );

        assert!(!result, "Should return false on EOF");
        assert!(guidance.is_empty(), "Must not record guidance on EOF");
    }

    #[test]
    fn test_handle_human_review_timeout_none_means_blocking_read() {
        // timeout=None: reads input normally (does not immediately return).
        let input = "blocking feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result = handle_human_review(cursor, "TASK-123", "title", None, 1, &mut guidance, None);

        assert!(result, "timeout=None must read input and return true");
        assert!(!guidance.is_empty());
    }

    #[test]
    fn test_handle_human_review_timeout_zero_means_blocking_read() {
        // timeout=Some(0): treated as blocking (same as None), not an immediate return.
        let input = "zero timeout feedback\n\n";
        let cursor = io::Cursor::new(input);
        let mut guidance = SessionGuidance::new();

        let result =
            handle_human_review(cursor, "TASK-123", "title", None, 1, &mut guidance, Some(0));

        assert!(
            result,
            "timeout=Some(0) must read input and return true (blocking)"
        );
        assert!(!guidance.is_empty());
    }

    // --- format_human_review_banner tests (require FEAT-004) ---

    #[test]
    fn test_human_review_banner_includes_task_id_title_and_notes() {
        let banner = format_human_review_banner(
            "TASK-456",
            "Deploy the feature",
            Some("Check that database migrations ran successfully"),
        );

        assert!(
            banner.contains("TASK-456"),
            "Banner must include task ID; got: '{banner}'"
        );
        assert!(
            banner.contains("Deploy the feature"),
            "Banner must include task title; got: '{banner}'"
        );
        assert!(
            banner.contains("Check that database migrations ran successfully"),
            "Banner must include notes; got: '{banner}'"
        );
    }

    #[test]
    fn test_human_review_banner_includes_task_id_and_title_without_notes() {
        let banner = format_human_review_banner("TASK-123", "Some task title", None);

        assert!(banner.contains("TASK-123"), "Banner must include task ID");
        assert!(
            banner.contains("Some task title"),
            "Banner must include task title"
        );
        // notes=None: must not panic and should not include a notes section with garbage
    }

    // --- Run records + loop stop (FEAT-003) ---

    fn argv(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    fn write_test_record(db_dir: &Path, name: &str, record: &RunRecord) {
        let dir = loop_runs_dir(db_dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
    }

    fn sample_loop_record(canonical: &Path, pid: u32) -> RunRecord {
        RunRecord {
            pid,
            kind: RunRecordKind::Loop,
            prefix: Some("P1".to_string()),
            started_at_unix_secs: 1_700_000_000,
            canonical_tasks_dir: canonical.to_path_buf(),
            cwd: PathBuf::from("/tmp/cwd"),
            worktree: Some(PathBuf::from("/tmp/wt")),
            main_checkout: Some(PathBuf::from("/tmp/main")),
            extra_tasks_dirs: vec![PathBuf::from("/tmp/extra/tasks")],
        }
    }

    fn sample_batch_record(canonical: &Path, pid: u32) -> RunRecord {
        RunRecord {
            pid,
            kind: RunRecordKind::Batch,
            prefix: None,
            started_at_unix_secs: 1_700_000_000,
            canonical_tasks_dir: canonical.to_path_buf(),
            cwd: PathBuf::from("/tmp/cwd"),
            worktree: None,
            main_checkout: Some(PathBuf::from("/tmp/main")),
            extra_tasks_dirs: vec![],
        }
    }

    #[test]
    fn cmdline_qualifies_task_mgr_loop_run() {
        assert!(cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/usr/bin/task-mgr",
            "loop",
            "run",
            "prd.json"
        ])));
        assert!(cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/usr/bin/task-mgr",
            "batch",
            "run",
            "tasks/*.json"
        ])));
    }

    #[test]
    fn cmdline_rejects_list_helper_flat_and_substring() {
        // Known-bad: kill -0 + substring "task-mgr" would accept these.
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/usr/bin/task-mgr",
            "list"
        ])));
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/opt/task-mgr-helper",
            "loop",
            "run"
        ])));
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/opt/not-task-mgr",
            "loop",
            "run"
        ])));
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/opt/task-mgr-wrapper/loop",
            "run"
        ])));
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "task-mgr loop-run"
        ])));
        // Deprecated flat form: no separate `run` token.
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/usr/bin/task-mgr",
            "loop",
            "prd.json"
        ])));
        assert!(!cmdline_qualifies_as_loop_or_batch_run(&argv(&[
            "/usr/bin/task-mgr",
            "loop",
            "stop",
            "--prefix",
            "P1"
        ])));
    }

    #[test]
    fn parse_proc_cmdline_splits_on_nul() {
        let raw = b"/usr/bin/task-mgr\0loop\0run\0prd.json\0";
        let parts = parse_proc_cmdline(raw);
        assert_eq!(
            parts,
            argv(&["/usr/bin/task-mgr", "loop", "run", "prd.json"])
        );
    }

    #[test]
    fn loop_stop_live_prefix_writes_prefix_stop_not_global() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let canonical = tmp.path().join("tasks");
        fs::create_dir_all(&canonical).unwrap();
        let pid = std::process::id();
        write_test_record(&db_dir, "P1.json", &sample_loop_record(&canonical, pid));
        // Also plant a batch record — live prefix must win and must not write global.
        let batch_canonical = db_dir.join("tasks");
        fs::create_dir_all(&batch_canonical).unwrap();
        write_test_record(
            &db_dir,
            "batch.json",
            &sample_batch_record(&batch_canonical, pid),
        );

        let good = argv(&["/usr/bin/task-mgr", "loop", "run"]);
        let written = loop_stop_with(&db_dir, "P1", |_| true, |_| Some(good.clone())).unwrap();

        assert_eq!(written, canonical.join(".stop-P1"));
        assert!(written.exists());
        assert!(
            !batch_canonical.join(".stop").exists(),
            "live prefix must not create global .stop even when batch.json is present"
        );
        assert!(
            !tmp.path().join("tasks").join(".stop").exists()
                || written == canonical.join(".stop-P1"),
            "must not invent a cwd-relative global stop"
        );
    }

    #[test]
    fn loop_stop_dead_pid_creates_no_file() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let canonical = tmp.path().join("tasks");
        fs::create_dir_all(&canonical).unwrap();
        write_test_record(
            &db_dir,
            "P1.json",
            &sample_loop_record(&canonical, 9_999_999),
        );

        let good = argv(&["/usr/bin/task-mgr", "loop", "run"]);
        let err = loop_stop_with(&db_dir, "P1", |_| false, |_| Some(good.clone())).unwrap_err();
        assert!(!canonical.join(".stop-P1").exists());
        assert!(
            matches!(err, TaskMgrError::NotFound { .. }),
            "dead pid must fail closed: {err}"
        );
    }

    #[test]
    fn loop_stop_rejects_list_and_helper_cmdlines() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let canonical = tmp.path().join("tasks");
        fs::create_dir_all(&canonical).unwrap();
        let pid = std::process::id();
        write_test_record(&db_dir, "P1.json", &sample_loop_record(&canonical, pid));

        for bad in [
            argv(&["/usr/bin/task-mgr", "list"]),
            argv(&["/opt/task-mgr-helper", "loop", "run"]),
            argv(&["/opt/not-task-mgr", "loop", "run"]),
            argv(&["task-mgr loop-run"]),
            argv(&["/usr/bin/task-mgr", "loop", "prd.json"]),
        ] {
            let err = loop_stop_with(&db_dir, "P1", |_| true, {
                let bad = bad.clone();
                move |_| Some(bad.clone())
            })
            .unwrap_err();
            assert!(
                !canonical.join(".stop-P1").exists(),
                "bad cmdline {:?} must create no file; err={err}",
                bad
            );
        }
    }

    #[test]
    fn loop_stop_recheck_unlinks_only_written_path() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let canonical = tmp.path().join("tasks");
        fs::create_dir_all(&canonical).unwrap();
        let pid = std::process::id();
        write_test_record(&db_dir, "P1.json", &sample_loop_record(&canonical, pid));

        // Pre-existing stop/pause files that must survive a failed recheck unlink.
        let other_stop = canonical.join(".stop-OTHER");
        let pause = canonical.join(".pause-P1");
        let global = canonical.join(".stop");
        fs::write(&other_stop, b"").unwrap();
        fs::write(&pause, b"").unwrap();
        fs::write(&global, b"").unwrap();

        let good = argv(&["/usr/bin/task-mgr", "loop", "run"]);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let err = loop_stop_with(
            &db_dir,
            "P1",
            |_| true,
            |_| {
                // First call (pre-write) qualifies; second (recheck) looks dead.
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Some(good.clone())
                } else {
                    Some(argv(&["/usr/bin/task-mgr", "list"]))
                }
            },
        )
        .unwrap_err();

        assert!(
            !canonical.join(".stop-P1").exists(),
            "recheck failure must unlink the path this command wrote"
        );
        assert!(other_stop.exists(), "must not unlink other prefix stop");
        assert!(pause.exists(), "must not unlink pause file");
        assert!(global.exists(), "must not unlink global stop");
        assert!(
            matches!(err, TaskMgrError::InvalidState { .. }),
            "recheck fail is InvalidState: {err}"
        );
    }

    #[test]
    fn loop_stop_invalid_prefix_before_path_join() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        // No loop-runs dir — invalid prefix must fail before creating anything.
        for bad in ["../x", "", "has/slash", "has space"] {
            let err = loop_stop_with(db_dir.as_path(), bad, |_| true, |_| None).unwrap_err();
            assert!(
                matches!(err, TaskMgrError::InvalidConfig { .. }),
                "prefix {bad:?} must be InvalidConfig: {err}"
            );
            assert!(
                !loop_runs_dir(&db_dir).exists(),
                "invalid prefix must not create loop-runs/"
            );
            assert!(
                !tmp.path().join("tasks").exists(),
                "invalid prefix must not create tasks/ under tmp"
            );
        }
    }

    #[test]
    fn loop_stop_batch_only_writes_global_stop() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let batch_canonical = db_dir.join("tasks");
        fs::create_dir_all(&batch_canonical).unwrap();
        let pid = std::process::id();
        write_test_record(
            &db_dir,
            "batch.json",
            &sample_batch_record(&batch_canonical, pid),
        );

        let good = argv(&["/usr/bin/task-mgr", "batch", "run"]);
        let written = loop_stop_with(&db_dir, "P1", |_| true, |_| Some(good.clone())).unwrap();

        assert_eq!(written, batch_canonical.join(".stop"));
        assert!(written.exists());
        assert!(
            !batch_canonical.join(".stop-P1").exists(),
            "batch-only must not invent a prefix stop file"
        );
    }

    #[test]
    fn loop_stop_no_record_creates_nothing_in_cwd() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).unwrap();
        let cwd_tasks = tmp.path().join("tasks");
        fs::create_dir_all(&cwd_tasks).unwrap();

        let err = loop_stop_with(&db_dir, "P1", |_| true, |_| None).unwrap_err();
        assert!(matches!(err, TaskMgrError::NotFound { .. }));
        assert!(!cwd_tasks.join(".stop").exists());
        assert!(!cwd_tasks.join(".stop-P1").exists());
        assert!(!db_dir.join("tasks").join(".stop").exists());
    }

    #[test]
    fn write_and_delete_loop_run_record_round_trip() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let canonical = tmp.path().join("tasks");
        fs::create_dir_all(&canonical).unwrap();
        let locs = SignalLocations {
            canonical: canonical.clone(),
            extras: vec![tmp.path().join("extra/tasks")],
            started_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(42),
        };
        write_loop_run_record(
            &db_dir,
            "P1",
            &locs,
            Path::new("/tmp/cwd"),
            Some(Path::new("/tmp/wt")),
            Some(Path::new("/tmp/main")),
        )
        .unwrap();
        let path = loop_run_record_path(&db_dir, "P1").unwrap();
        assert!(path.exists());
        let loaded: RunRecord = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.kind, RunRecordKind::Loop);
        assert_eq!(loaded.prefix.as_deref(), Some("P1"));
        assert_eq!(loaded.canonical_tasks_dir, canonical);
        assert_eq!(loaded.started_at_unix_secs, 42);

        delete_loop_run_record(&db_dir, Some("P1"));
        assert!(!path.exists());
    }

    #[test]
    fn batch_run_record_guard_deletes_on_drop() {
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join(".task-mgr");
        let canonical = db_dir.join("tasks");
        fs::create_dir_all(&canonical).unwrap();
        let locs = SignalLocations {
            canonical,
            extras: vec![],
            started_at: SystemTime::UNIX_EPOCH,
        };
        let path = batch_run_record_path(&db_dir);
        {
            let _guard =
                BatchRunRecordGuard::write(&db_dir, &locs, Path::new("/tmp/cwd"), None).unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists(), "Drop must delete batch.json");
    }
}
