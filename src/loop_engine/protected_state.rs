//! Protected state guard for Codex sandbox writes.
//!
//! Codex spawns with its `--cd` cwd plus `/tmp` writable; the loop's worktree
//! cwd contains orchestrator state (task JSON files, prompt files,
//! `.last-branch`, and the SQLite trio under `db_dir`). This module DETECTS AND
//! REVERTS post-hoc — TOCTOU between snapshot and verify is inherent. It does
//! not prevent in-run corruption.
//!
//! # Two file classes
//!
//! - **Text artifacts** under `tasks_dir` (`**/*.json`, `**/*-prompt.md`,
//!   `.last-branch` at the top level): snapshot file content + (dev, inode);
//!   on verify a content delta OR an inode change OR a new file matching the
//!   protected globs is treated as a Codex mutation and reverted. The loop
//!   engine and the `task-mgr` CLI do not write to these paths during a Codex
//!   iteration, so byte-exact restore is correct.
//!
//! - **SQLite trio** at `db_dir/{tasks.db, tasks.db-wal, tasks.db-shm}`:
//!   INTEGRITY-based detection, NOT byte hashing. Codex iterations may
//!   legitimately run `task-mgr` subcommands that mutate the DB through valid
//!   SQL; those writes preserve `PRAGMA quick_check` and `global_state.schema_version`.
//!   A quick_check failure, an open failure, or a schema_version regression is
//!   FATAL — the loop stops and we do NOT attempt a raw byte restore (a
//!   byte-overwrite of an active WAL/SHM trio risks worse corruption than the
//!   original event).
//!
//! # Escape protection
//!
//! Snapshot reads use [`std::fs::symlink_metadata`] so a hostile regular →
//! symlink swap is observed via the inode change. On restore, an existing
//! symlink at the target path is removed BEFORE the regular-file write so the
//! snapshot bytes are never written through the link target. Parent
//! directories are canonicalized and confined to `tasks_dir` before any
//! [`std::fs::create_dir_all`]-equivalent call — an attacker-planted symlink
//! at an intermediate component cannot redirect a write outside the protected
//! tree.
//!
//! # TOCTOU window bounds
//!
//! The TOCTOU window spans three phases: **snapshot** (state captured before
//! spawn), the **Codex run** (any observer or racing writer can see mutated
//! state here), and **verify** (deltas compared post-exit). A fourth gap
//! exists **after** verify but **before** the next snapshot: mutations landing
//! in that window — e.g. a background process or a concurrent loop iteration
//! writing to the worktree — are not attributable to the guarded Codex run and
//! will not be reverted by the current guard cycle.
//!
//! # walk_protected fail-open on unreadable directories
//!
//! [`walk_protected`] (used during snapshot and new-file detection) calls
//! [`std::path::Path::canonicalize`] on every subdirectory to verify
//! containment before descending. If `canonicalize` returns an error — for
//! example, `EACCES` because Codex `chmod 000`'d a subdirectory — the
//! function emits a [`tracing::warn!`] and **skips descent** (see
//! `protected_state.rs` lines 550–564). Files planted beneath an unreadable
//! directory therefore **escape new-file detection**; the inode/dev/content
//! checks on already-snapshotted files still fire, but a brand-new file added
//! under the chmod'd tree will not be caught or reverted.
//!
//! # Guard gate
//!
//! [`runner_requires_state_guard`] is a positive allowlist — `true` only for
//! [`RunnerKind::Codex`] in v1. Claude/Grok callers route through a `None`
//! snapshot (via [`Snapshot::take`]) and the verify barrier becomes a no-op,
//! so non-Codex runs are byte-identical to HEAD.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::runner::RunnerKind;
use crate::output::ui;

/// Filename of the loop's branch-bookmark file. Lives directly under
/// `tasks_dir` (`src/loop_engine/mod.rs::LAST_BRANCH_FILE`). Duplicated here
/// because the module-level const is `pub(crate)` and importing it would
/// create a circular link between `mod.rs` and this module; the contract test
/// `last_branch_filename_matches_mod_constant` pins the two together so a
/// future rename to either site fails at unit-test time.
const LAST_BRANCH_FILE: &str = ".last-branch";

/// Suffix used to identify per-PRD prompt files
/// (`tasks/<prd>-prompt.md` — see `env.rs::resolve_paths`).
const PROMPT_SUFFIX: &str = "-prompt.md";

/// Positive allowlist controlling whether the state guard runs at all.
///
/// Returns `true` only for runners whose sandbox can mutate orchestrator
/// state inside the loop's worktree (Codex in v1). Other runners (Claude,
/// Grok) route through a `None` snapshot from [`Snapshot::take`] so verify
/// is a no-op and the spawn path stays byte-identical to HEAD.
///
/// Exhaustive `match` (no `_` arm): adding a new [`RunnerKind`] variant
/// becomes a compile-error here so every new runner makes a deliberate
/// sandbox-trust decision.
pub(crate) fn runner_requires_state_guard(kind: RunnerKind) -> bool {
    match kind {
        RunnerKind::Codex => true,
        RunnerKind::Claude | RunnerKind::Grok => false,
    }
}

/// Per-file recorded state for the protected text artifacts.
#[derive(Debug, Clone)]
struct FileSnapshot {
    content: Vec<u8>,
    dev: u64,
    inode: u64,
}

/// Recorded state captured immediately before a guarded subprocess spawns.
///
/// Holds:
/// - Per-file snapshots for every text artifact under `tasks_dir` that
///   matches the protected globs at snapshot time.
/// - A baseline `schema_version` for the SQLite database (queried from
///   `global_state` when the table exists). The SQLite files themselves
///   are NOT byte-snapshotted — see module-level docs.
///
/// `take_unconditional` is exposed for unit tests that need to exercise the
/// guard without going through the runner gate; production callers use
/// [`Snapshot::take`].
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub(crate) tasks_dir: PathBuf,
    pub(crate) db_dir: PathBuf,
    files: HashMap<PathBuf, FileSnapshot>,
    /// Baseline `global_state.schema_version`. `None` when the table is
    /// absent (uninitialised DB) — `verify_and_restore` skips the regression
    /// check in that case.
    baseline_schema_version: Option<i64>,
}

/// Outcome of one verify-and-restore pass.
#[derive(Debug)]
pub(crate) enum VerifyOutcome {
    /// Every protected file matches its snapshot, no new file appeared under
    /// a protected glob, and SQLite integrity passes. The loop continues.
    Clean,
    /// Text mutations and/or new-file plants were detected and reverted.
    /// SQLite integrity still passed. The loop continues; the operator may
    /// want to audit the run.
    Reverted {
        restored: Vec<PathBuf>,
        removed_new_files: Vec<PathBuf>,
        /// Non-empty when one or more individual restore actions failed.
        /// The mutation was still detected, just not fully repaired — the
        /// caller decides whether to escalate.
        errors: Vec<RestoreError>,
    },
    /// SQLite integrity check failed, the DB did not open, or
    /// `schema_version` regressed. The loop MUST stop. No raw restore is
    /// attempted — overwriting an active WAL/SHM trio with snapshot bytes
    /// could worsen the corruption.
    FatalSqliteCorruption { path: PathBuf, reason: String },
}

/// Single restore-action failure surfaced inside [`VerifyOutcome::Reverted`].
#[derive(Debug, Clone)]
pub(crate) struct RestoreError {
    pub path: PathBuf,
    pub message: String,
}

/// Shared call-site adapter: run a snapshot's `verify_and_restore` and fold
/// the tri-state outcome into the loop's `TaskMgrResult<()>` contract.
///
/// - `Clean` → `Ok(())` silently (no operator noise on the common path).
/// - `Reverted` → `Ok(())` after emitting an operator-visible summary of
///   the restored / removed / errored paths. The loop continues — text
///   mutations are repaired, not promoted to fatal.
/// - `FatalSqliteCorruption` → `Err(TaskMgrError::ProtectedTaskStateMutation { fatal: true, … })`
///   so the iteration/slot aborts via `?`. The caller decides whether to
///   translate this into a slot-level `Crash` or a wave-level terminal.
///
/// The wave path bypasses this helper because it needs to fold the fatal
/// branch into a `WaveOutcome` with a synthetic `FailedMerge` (cascade-halt
/// streak preservation). See `wave_scheduler::run_wave_iteration`.
pub(crate) fn apply_verify_outcome(snapshot: &Snapshot, site: &'static str) -> TaskMgrResult<()> {
    match snapshot.verify_and_restore() {
        VerifyOutcome::Clean => Ok(()),
        VerifyOutcome::Reverted {
            restored,
            removed_new_files,
            errors,
        } => {
            ui::emit(&format!(
                "[protected-state/{site}] Codex iteration mutated task state: restored {} file(s), removed {} planted file(s), {} restore error(s)",
                restored.len(),
                removed_new_files.len(),
                errors.len()
            ));
            for p in &restored {
                ui::emit(&format!(
                    "[protected-state/{site}]   restored: {}",
                    p.display()
                ));
            }
            for p in &removed_new_files {
                ui::emit(&format!(
                    "[protected-state/{site}]   removed:  {}",
                    p.display()
                ));
            }
            for e in &errors {
                ui::emit(&format!(
                    "[protected-state/{site}]   restore-error {}: {}",
                    e.path.display(),
                    e.message
                ));
            }
            Ok(())
        }
        VerifyOutcome::FatalSqliteCorruption { path, reason } => {
            Err(TaskMgrError::ProtectedTaskStateMutation {
                runner_kind: RunnerKind::Codex,
                paths: vec![format!("{}: {}", path.display(), reason)],
                fatal: true,
            })
        }
    }
}

impl Snapshot {
    /// Capture protected state when the upcoming spawn needs guarding.
    ///
    /// Returns `None` when `kind` does not require the guard
    /// ([`runner_requires_state_guard`] returns `false`). Gate every
    /// protected_state call site on the returned `Option` so non-Codex runs
    /// stay byte-identical to HEAD.
    pub(crate) fn take(db_dir: &Path, tasks_dir: &Path, kind: RunnerKind) -> Option<Self> {
        if !runner_requires_state_guard(kind) {
            return None;
        }
        Some(Self::take_unconditional(db_dir, tasks_dir))
    }

    /// Capture protected state unconditionally — for unit tests that exercise
    /// the snapshot/verify logic without routing through the runner gate.
    pub(crate) fn take_unconditional(db_dir: &Path, tasks_dir: &Path) -> Self {
        let files = collect_protected_files(tasks_dir);
        let baseline_schema_version = read_schema_version(db_dir);
        Self {
            tasks_dir: tasks_dir.to_path_buf(),
            db_dir: db_dir.to_path_buf(),
            files,
            baseline_schema_version,
        }
    }

    /// Compare on-disk state to the snapshot, restoring text artifacts on
    /// mismatch and reporting SQLite integrity failures as fatal.
    ///
    /// Order of operations:
    /// 1. SQLite integrity check — fatal short-circuits before any text
    ///    restore so a corrupted DB never gets compounded by extra writes
    ///    while the operator is paged.
    /// 2. Per-file verify-and-restore for each snapshotted path. Symlink
    ///    swaps are detected via inode change AND the restore removes the
    ///    link before writing the regular file (preventing arbitrary write
    ///    through an attacker-planted target).
    /// 3. New-file detection — walks `tasks_dir` for paths matching the
    ///    protected globs that did not exist at snapshot time and removes
    ///    them.
    pub(crate) fn verify_and_restore(&self) -> VerifyOutcome {
        if let Some(fatal) = check_sqlite_integrity(&self.db_dir, self.baseline_schema_version) {
            return fatal;
        }

        let mut restored: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<RestoreError> = Vec::new();

        for (path, snap) in &self.files {
            match verify_and_restore_one(path, snap, &self.tasks_dir) {
                RestoreAction::Unchanged => {}
                RestoreAction::Restored => restored.push(path.clone()),
                RestoreAction::Failed(err) => errors.push(err),
            }
        }

        let mut removed_new_files: Vec<PathBuf> = Vec::new();
        for path in current_protected_files(&self.tasks_dir) {
            if self.files.contains_key(&path) {
                continue;
            }
            match remove_planted_path(&path) {
                Ok(()) => removed_new_files.push(path),
                Err(e) => errors.push(RestoreError {
                    path,
                    message: format!("remove planted: {}", e),
                }),
            }
        }

        if restored.is_empty() && removed_new_files.is_empty() && errors.is_empty() {
            VerifyOutcome::Clean
        } else {
            VerifyOutcome::Reverted {
                restored,
                removed_new_files,
                errors,
            }
        }
    }
}

/// Per-file outcome inside [`Snapshot::verify_and_restore`].
enum RestoreAction {
    Unchanged,
    Restored,
    Failed(RestoreError),
}

fn verify_and_restore_one(path: &Path, snap: &FileSnapshot, tasks_dir: &Path) -> RestoreAction {
    // Check whether an intermediate path component was swapped for a symlink
    // that escapes tasks_dir. `symlink_metadata` on the FINAL component does
    // not detect this — the kernel follows intermediate symlinks silently, so
    // an attacker who stages identical content at the link target fools the
    // dev/inode/content checks. Canonicalizing the parent catches it.
    let parent_escaped = path.parent().is_some_and(|parent| {
        match (tasks_dir.canonicalize(), parent.canonicalize()) {
            (Ok(tasks_real), Ok(real_parent)) => !real_parent.starts_with(&tasks_real),
            _ => true, // canonicalize failure → treat as escaped (fail safe)
        }
    });

    let current = fs::symlink_metadata(path);

    let needs_restore = parent_escaped
        || match &current {
            Err(_) => true,
            Ok(meta) => {
                let ft = meta.file_type();
                // (a) Any non-regular file is a mutation candidate (symlink swap,
                //     directory replacement, special-file plant); (b) an inode
                //     change indicates a hardlink retarget; (c) otherwise a byte
                //     compare catches a same-inode content edit. All three rungs
                //     flow into the same restore.
                !ft.is_file()
                    || ft.is_symlink()
                    || meta.dev() != snap.dev
                    || meta.ino() != snap.inode
                    || fs::read(path)
                        .map(|bytes| bytes != snap.content)
                        .unwrap_or(true)
            }
        };

    if !needs_restore {
        return RestoreAction::Unchanged;
    }

    if parent_escaped {
        // An intermediate component is a symlink that escapes tasks_dir.
        // Remove the offending symlink from tasks_dir downward before
        // proceeding — this unlinks the attacker's escape route and lets
        // ensure_contained_parent recreate the real directory tree.
        if let Err(e) = remove_escaped_intermediate(path, tasks_dir) {
            return RestoreAction::Failed(RestoreError {
                path: path.to_path_buf(),
                message: format!("remove escaped intermediate: {}", e),
            });
        }
    } else {
        // De-symlink BEFORE write. `fs::remove_file` on a symlink removes the
        // LINK itself, not the target. After this step the path is either
        // absent or a regular file (a contained one — see parent containment
        // below); `fs::write` then produces a regular file at the expected
        // location instead of writing through the link.
        if let Ok(meta) = &current {
            let ft = meta.file_type();
            if ft.is_symlink() || (!ft.is_file()) {
                let removal = if ft.is_dir() {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                };
                if let Err(e) = removal {
                    return RestoreAction::Failed(RestoreError {
                        path: path.to_path_buf(),
                        message: format!("remove pre-existing non-regular: {}", e),
                    });
                }
            }
        }
    }

    if let Some(parent) = path.parent()
        && let Err(e) = ensure_contained_parent(parent, tasks_dir)
    {
        return RestoreAction::Failed(RestoreError {
            path: path.to_path_buf(),
            message: format!("ensure parent: {}", e),
        });
    }

    // Write through the canonicalized parent + filename. After
    // ensure_contained_parent returns Ok the parent exists and is confined,
    // so canonicalize() must succeed. This closes the TOCTOU window that
    // would open if a future caller invoked verify mid-spawn rather than
    // post-join (currently prohibited: verify always runs after Codex joins).
    let write_path = if let Some(parent) = path.parent() {
        match parent.canonicalize() {
            Ok(canon) => {
                if let Some(name) = path.file_name() {
                    canon.join(name)
                } else {
                    path.to_path_buf()
                }
            }
            Err(e) => {
                return RestoreAction::Failed(RestoreError {
                    path: path.to_path_buf(),
                    message: format!("canonicalize parent post-ensure: {}", e),
                });
            }
        }
    } else {
        path.to_path_buf()
    };

    match fs::write(&write_path, &snap.content) {
        Ok(()) => RestoreAction::Restored,
        Err(e) => RestoreAction::Failed(RestoreError {
            path: path.to_path_buf(),
            message: format!("write: {}", e),
        }),
    }
}

/// Walk path components from `tasks_dir` downward until the first symlink is
/// found, then remove it via `fs::remove_file` (which unlinks the symlink
/// itself, not its target).
///
/// Called when an intermediate component is known to escape `tasks_dir`;
/// removing the link lets [`ensure_contained_parent`] recreate the real
/// directory and [`fs::write`] land at the correct location.
fn remove_escaped_intermediate(path: &Path, tasks_dir: &Path) -> std::io::Result<()> {
    let relative = path.strip_prefix(tasks_dir).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not under tasks_dir",
        )
    })?;
    let mut current = tasks_dir.to_path_buf();
    for component in relative.components() {
        current = current.join(component);
        if !current.exists() {
            // Component is absent — a prior step already removed the link or
            // the path never existed from here on. Nothing to do.
            return Ok(());
        }
        let meta = fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() {
            return fs::remove_file(&current);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no escaped intermediate symlink found between tasks_dir and path",
    ))
}

/// Canonicalize-and-contain the parent of a restore target, creating it if
/// absent. Refuses any operation where the realpath of the parent (or the
/// first existing ancestor) escapes `tasks_dir` — an attacker-planted
/// symlink in an intermediate path component cannot then redirect a write
/// outside the protected tree.
fn ensure_contained_parent(parent: &Path, tasks_dir: &Path) -> std::io::Result<()> {
    let tasks_real = tasks_dir.canonicalize()?;
    if parent.exists() {
        let real_parent = parent.canonicalize()?;
        if !real_parent.starts_with(&tasks_real) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "parent {} escapes tasks_dir {}",
                    real_parent.display(),
                    tasks_real.display()
                ),
            ));
        }
        return Ok(());
    }

    // Walk up to the first existing ancestor and verify ITS canonical path
    // lies inside tasks_dir; then create the missing children one at a time.
    let mut to_create: Vec<&Path> = Vec::new();
    let mut existing = parent;
    while !existing.exists() {
        to_create.push(existing);
        match existing.parent() {
            Some(p) => existing = p,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "no existing ancestor under tasks_dir",
                ));
            }
        }
    }
    let real_existing = existing.canonicalize()?;
    if !real_existing.starts_with(&tasks_real) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "ancestor {} escapes tasks_dir {}",
                real_existing.display(),
                tasks_real.display()
            ),
        ));
    }
    for child in to_create.iter().rev() {
        fs::create_dir(child)?;
    }
    Ok(())
}

/// Remove a Codex-planted path matched by a protected glob. Uses
/// `symlink_metadata` so a hostile symlink target is never followed.
fn remove_planted_path(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Walk `tasks_dir` and return every existing path that matches a protected
/// glob (regular files OR symlinks at protected names). Symlinks are
/// surfaced so the new-file pass can remove planted symlinks too. Hidden
/// `.task-mgr/` infrastructure dirs are intentionally not skipped — the
/// loop's `tasks_dir` is `<db_dir>/tasks/`, which has no nested infra.
fn current_protected_files(tasks_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_protected(tasks_dir, tasks_dir, &mut out);
    out
}

fn walk_protected(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            if matches_protected_glob(root, &path) {
                out.push(path);
            }
            continue;
        }
        if ft.is_dir() {
            // Refuse to descend if the directory's canonical path escapes
            // tasks_dir — defense-in-depth against any intermediate symlink
            // the OS may have followed in a prior path component.
            match (root.canonicalize(), path.canonicalize()) {
                (Ok(tasks_real), Ok(real_path)) => {
                    if real_path.starts_with(&tasks_real) {
                        walk_protected(root, &path, out);
                    }
                }
                // Canonicalize failure (e.g. EACCES on a Codex-chmod'd subdir)
                // is logged rather than silently skipped. A planted file beneath
                // an unreadable directory would otherwise escape new-file
                // detection. Matches verify_and_restore_one's safe-fail posture.
                (_, Err(e)) => tracing::warn!(
                    "walk_protected: cannot verify containment of {}: {} — descent skipped, planted files may escape detection",
                    path.display(),
                    e
                ),
                (Err(e), Ok(_)) => tracing::warn!(
                    "walk_protected: cannot canonicalize tasks root {}: {} — descent skipped",
                    root.display(),
                    e
                ),
            }
            continue;
        }
        if matches_protected_glob(root, &path) {
            out.push(path);
        }
    }
}

/// Predicate for the protected glob set:
/// - `**/*.json` (any depth under `tasks_dir`)
/// - `**/*-prompt.md` (any depth under `tasks_dir`)
/// - `<tasks_dir>/.last-branch` (top level only)
fn matches_protected_glob(tasks_dir: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if name.ends_with(".json") {
        return true;
    }
    if name.ends_with(PROMPT_SUFFIX) {
        return true;
    }
    if name == LAST_BRANCH_FILE
        && let Ok(rel) = path.strip_prefix(tasks_dir)
    {
        return rel == Path::new(LAST_BRANCH_FILE);
    }
    false
}

fn collect_protected_files(tasks_dir: &Path) -> HashMap<PathBuf, FileSnapshot> {
    let mut out: HashMap<PathBuf, FileSnapshot> = HashMap::new();
    for path in current_protected_files(tasks_dir) {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        // Skip non-regular at snapshot time: production tooling never starts
        // with a symlink at a protected path, and snapshotting a symlink's
        // metadata as "baseline" would mask a subsequent swap.
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(content) = fs::read(&path) else {
            continue;
        };
        out.insert(
            path,
            FileSnapshot {
                content,
                dev: meta.dev(),
                inode: meta.ino(),
            },
        );
    }
    out
}

fn query_schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT schema_version FROM global_state WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

/// Query `global_state.schema_version`. Returns `None` when the DB or the
/// table is absent (uninitialised). The downstream regression check skips
/// when the baseline is `None`.
///
/// Opens with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI` and `immutable=1`
/// so SQLite never attempts WAL recovery or writes to any of the trio files
/// — this function is called at snapshot time and must not mutate state.
fn read_schema_version(db_dir: &Path) -> Option<i64> {
    let db_path = db_dir.join("tasks.db");
    if !db_path.exists() {
        return None;
    }
    let uri = format!("file:{}?mode=ro&immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    query_schema_version(&conn).ok()
}

/// Run the SQLite integrity checks. Returns `Some(FatalSqliteCorruption)`
/// when the database disappeared, will not open, fails `PRAGMA quick_check`,
/// or regressed `schema_version` below the snapshot baseline.
fn check_sqlite_integrity(
    db_dir: &Path,
    baseline_schema_version: Option<i64>,
) -> Option<VerifyOutcome> {
    let db_path = db_dir.join("tasks.db");

    if !db_path.exists() {
        // The DB was present at snapshot time (a baseline was captured) but
        // is gone now — Codex deleted or moved it. Treat as fatal.
        return baseline_schema_version
            .is_some()
            .then(|| VerifyOutcome::FatalSqliteCorruption {
                path: db_path.clone(),
                reason: "tasks.db disappeared between snapshot and verify".to_string(),
            });
    }

    // Open read-only with immutable=1 so SQLite never attempts WAL recovery
    // or writes to any file in the trio. Verification must not compound
    // corruption by replaying a Codex-stomped WAL into the main DB.
    let uri = format!("file:{}?mode=ro&immutable=1", db_path.display());
    let conn = match Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Some(VerifyOutcome::FatalSqliteCorruption {
                path: db_path,
                reason: format!("open failed: {}", e),
            });
        }
    };

    // PRAGMA quick_check returns "ok" on a healthy DB; otherwise it returns
    // one or more diagnostic lines. quick_check is preferred over
    // integrity_check because it omits the expensive cross-index walk and
    // still catches every form of raw corruption (truncated pages, bad
    // headers, busted page links).
    let qc: Result<String, _> = conn.pragma_query_value(None, "quick_check", |row| row.get(0));
    match qc {
        Ok(ref msg) if msg == "ok" => {}
        Ok(ref msg) if msg.contains("attempt to write a readonly database") => {
            // FTS5 quick_check internally needs write access for its shadow-
            // table bookkeeping; with immutable=1 this surfaces as a diagnostic
            // message (not a Rust error) even on a healthy DB. It is an FTS5
            // implementation limitation, not B-tree corruption — the open
            // succeeded, meaning the file header and page structure are intact.
            // Actual truncation/garbage is caught by the open-failure path
            // above; regressions are caught by the schema_version check below.
        }
        Ok(msg) => {
            return Some(VerifyOutcome::FatalSqliteCorruption {
                path: db_path,
                reason: format!("quick_check: {}", msg),
            });
        }
        Err(e) => {
            return Some(VerifyOutcome::FatalSqliteCorruption {
                path: db_path,
                reason: format!("quick_check query failed: {}", e),
            });
        }
    }

    if let Some(baseline) = baseline_schema_version {
        let current = query_schema_version(&conn);
        match current {
            Ok(v) if v >= baseline => {}
            Ok(v) => {
                return Some(VerifyOutcome::FatalSqliteCorruption {
                    path: db_path,
                    reason: format!("schema_version regressed from {} to {}", baseline, v),
                });
            }
            Err(e) => {
                return Some(VerifyOutcome::FatalSqliteCorruption {
                    path: db_path,
                    reason: format!("schema_version query failed: {}", e),
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_connection;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn make_dirs() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let db_dir = tmp.path().to_path_buf();
        let tasks_dir = db_dir.join("tasks");
        fs::create_dir_all(&tasks_dir).expect("mkdir tasks");
        // Initialise a real DB with the migrated schema so the integrity
        // check has something to verify against.
        let _ = open_connection(&db_dir).expect("open conn");
        crate::db::migrations::run_migrations(&mut open_connection(&db_dir).unwrap())
            .expect("migrate");
        (tmp, db_dir, tasks_dir)
    }

    // === guard gate ===

    #[test]
    fn guard_gate_codex_yes_claude_grok_no() {
        assert!(runner_requires_state_guard(RunnerKind::Codex));
        assert!(!runner_requires_state_guard(RunnerKind::Claude));
        assert!(!runner_requires_state_guard(RunnerKind::Grok));
    }

    #[test]
    fn take_returns_none_for_non_codex_runners() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        assert!(Snapshot::take(&db_dir, &tasks_dir, RunnerKind::Claude).is_none());
        assert!(Snapshot::take(&db_dir, &tasks_dir, RunnerKind::Grok).is_none());
        assert!(Snapshot::take(&db_dir, &tasks_dir, RunnerKind::Codex).is_some());
    }

    #[test]
    fn last_branch_filename_matches_mod_constant() {
        // Pin our local LAST_BRANCH_FILE to the canonical mod-level constant
        // so a future rename either side fails at unit-test time. Either rename
        // both, or none.
        assert_eq!(LAST_BRANCH_FILE, crate::loop_engine::LAST_BRANCH_FILE);
    }

    // === positive: text artifact mutation detected and restored ===

    #[test]
    fn json_mutation_is_detected_and_restored() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let prd = tasks_dir.join("foo.json");
        fs::write(&prd, b"{\"tasks\":[]}").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Simulate a Codex sandbox write that mutates the task list.
        fs::write(&prd, b"{\"tasks\":[\"evil\"]}").unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::Reverted {
                restored,
                removed_new_files,
                errors,
            } => {
                assert_eq!(restored, vec![prd.clone()]);
                assert!(removed_new_files.is_empty());
                assert!(errors.is_empty(), "unexpected restore errors: {:?}", errors);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        assert_eq!(fs::read(&prd).unwrap(), b"{\"tasks\":[]}");
    }

    #[test]
    fn prompt_md_mutation_is_detected_and_restored() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let prompt = tasks_dir.join("foo-prompt.md");
        fs::write(&prompt, b"# initial").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);
        fs::write(&prompt, b"# tampered").unwrap();
        match snap.verify_and_restore() {
            VerifyOutcome::Reverted { restored, .. } => {
                assert_eq!(restored, vec![prompt.clone()]);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        assert_eq!(fs::read(&prompt).unwrap(), b"# initial");
    }

    #[test]
    fn last_branch_mutation_is_detected_and_restored() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let lb = tasks_dir.join(LAST_BRANCH_FILE);
        fs::write(&lb, b"main\n").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);
        fs::write(&lb, b"feat/evil\n").unwrap();
        match snap.verify_and_restore() {
            VerifyOutcome::Reverted { restored, .. } => {
                assert_eq!(restored, vec![lb.clone()]);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        assert_eq!(fs::read(&lb).unwrap(), b"main\n");
    }

    // === positive: integrity-preserving SQLite changes are ALLOWED ===

    #[test]
    fn legitimate_sqlite_write_is_allowed() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Simulate a Codex `task-mgr complete` invocation that legitimately
        // mutates the DB via valid SQL — integrity remains intact.
        let conn = Connection::open(db_dir.join("tasks.db")).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
            rusqlite::params!["LEGIT-001", "x", "done", 1],
        )
        .unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::Clean => {}
            other => panic!("expected Clean (DB integrity preserved), got {:?}", other),
        }
    }

    // Known-bad guard: a read-only `task-mgr show` against the DB mutates the
    // -shm trio file under WAL journal mode. A byte-compare guard (V2's
    // previous shape) would flag this as fatal. With the integrity-only
    // check, it must NOT halt.
    #[test]
    fn readonly_db_open_mutates_shm_but_not_fatal() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        // Enable WAL so -shm/-wal files materialize.
        {
            let conn = Connection::open(db_dir.join("tasks.db")).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
            conn.execute(
                "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
                rusqlite::params!["SEED-001", "seed", "todo", 99],
            )
            .unwrap();
        }
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Read-only access by `task-mgr show` / similar tools opens the DB
        // and writes to -shm even with no SQL UPDATE. This must NOT halt.
        let conn = Connection::open(db_dir.join("tasks.db")).unwrap();
        let _count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        drop(conn);

        match snap.verify_and_restore() {
            VerifyOutcome::Clean => {}
            VerifyOutcome::Reverted { errors, .. } if errors.is_empty() => {
                // Acceptable: text restore is no-op; no SQLite halt.
            }
            other => panic!("expected Clean (read-only DB access), got {:?}", other),
        }
    }

    // === positive: SQLite corruption is fatal, no raw restore ===

    #[test]
    fn truncated_tasks_db_is_fatal_no_restore() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();

        // Drive INSERTs that leave uncommitted WAL data so the WAL file exists
        // when we corrupt the main DB. If verify_and_restore opens with the
        // default read-write mode, SQLite would attempt WAL recovery and
        // overwrite tasks.db with WAL data — defeating the "no raw restore"
        // invariant. The immutable=1 open must prevent that entirely.
        {
            let conn = Connection::open(db_dir.join("tasks.db")).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
            conn.execute(
                "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
                rusqlite::params!["WAL-SEED-001", "wal seed task", "todo", 99],
            )
            .unwrap();
            // Connection drops without an explicit checkpoint, leaving WAL data
            // potentially unreflected in the main file.
        }

        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Replace tasks.db with one byte of garbage so it can't even open as
        // a valid SQLite file. The WAL file may still be present on disk.
        fs::write(db_dir.join("tasks.db"), b"x").unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::FatalSqliteCorruption { path, reason } => {
                assert!(path.ends_with("tasks.db"));
                // Acceptable signals for a corrupted/truncated DB: quick_check
                // diagnostic, open failure, or a schema_version query failure
                // (the `global_state` table is gone). All three are fatal.
                assert!(
                    reason.contains("quick_check")
                        || reason.contains("open")
                        || reason.contains("schema_version"),
                    "reason should mention quick_check/open/schema_version: {}",
                    reason
                );
            }
            other => panic!("expected FatalSqliteCorruption, got {:?}", other),
        }

        // No raw restore — main DB file must remain exactly the corrupted bytes.
        // With immutable=1 the verify path must NOT replay the WAL or write any
        // bytes to tasks.db.
        assert_eq!(
            fs::read(db_dir.join("tasks.db")).unwrap(),
            b"x",
            "verify_and_restore must not modify the corrupted tasks.db (WAL replay guard)"
        );
    }

    #[test]
    fn deleted_tasks_db_is_fatal() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);
        fs::remove_file(db_dir.join("tasks.db")).unwrap();
        match snap.verify_and_restore() {
            VerifyOutcome::FatalSqliteCorruption { reason, .. } => {
                assert!(reason.contains("disappeared"));
            }
            other => panic!("expected FatalSqliteCorruption, got {:?}", other),
        }
    }

    // === escape protection: symlink swap detected; restore does NOT
    //     write through the link target ===

    #[test]
    fn regular_to_symlink_swap_detected_and_target_not_written_through() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let prd = tasks_dir.join("victim.json");
        fs::write(&prd, b"{\"original\":true}").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Stage a target OUTSIDE tasks_dir. If the restore wrote through
        // the symlink, this file's bytes would change to the snapshot
        // content.
        let outside = db_dir.join("attacker-target.txt");
        fs::write(&outside, b"DO_NOT_OVERWRITE").unwrap();

        // Swap the regular file for a symlink pointing at the attacker's
        // out-of-tree file.
        fs::remove_file(&prd).unwrap();
        symlink(&outside, &prd).unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::Reverted {
                restored, errors, ..
            } => {
                assert_eq!(restored, vec![prd.clone()]);
                assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }

        // The link target MUST be untouched — restoring through the symlink
        // would constitute an arbitrary-write primitive against the
        // attacker-controlled path.
        assert_eq!(
            fs::read(&outside).unwrap(),
            b"DO_NOT_OVERWRITE",
            "symlink target must NOT be written through during restore"
        );

        // The protected path is now a regular file again, carrying the
        // snapshot content (link removed, fresh write).
        let meta = fs::symlink_metadata(&prd).unwrap();
        assert!(meta.file_type().is_file());
        assert!(!meta.file_type().is_symlink());
        assert_eq!(fs::read(&prd).unwrap(), b"{\"original\":true}");
    }

    // === intermediate-dir symlink swap: detected even with identical content ===

    #[test]
    fn intermediate_dir_symlink_swap_detected_and_restored() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();

        // Protected file nested one level down.
        let sub = tasks_dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let prd = sub.join("foo.json");
        fs::write(&prd, b"{\"original\":true}").unwrap();

        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Attacker stages IDENTICAL content at an out-of-tree location.
        // A naive content-compare would see no difference and return Clean.
        let attacker = db_dir.join("attacker");
        fs::create_dir(&attacker).unwrap();
        let attacker_file = attacker.join("foo.json");
        fs::write(&attacker_file, b"{\"original\":true}").unwrap();

        // Replace the real subdirectory with a symlink to the attacker dir.
        fs::remove_dir_all(&sub).unwrap();
        symlink(&attacker, &sub).unwrap();

        // Intermediate symlink must be detected even with identical bytes.
        match snap.verify_and_restore() {
            VerifyOutcome::Reverted {
                restored,
                removed_new_files,
                errors,
            } => {
                assert_eq!(restored, vec![prd.clone()], "file must be restored");
                assert!(removed_new_files.is_empty());
                assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }

        // tasks_dir/sub is now a real directory — symlink was removed.
        let sub_meta = fs::symlink_metadata(&sub).unwrap();
        assert!(
            sub_meta.is_dir(),
            "sub must be a real directory after restore"
        );
        assert!(!sub_meta.file_type().is_symlink());

        // The protected file carries the snapshot bytes.
        assert_eq!(fs::read(&prd).unwrap(), b"{\"original\":true}");

        // The attacker's file was NOT written through — link target is untouched.
        assert_eq!(
            fs::read(&attacker_file).unwrap(),
            b"{\"original\":true}",
            "attacker file must not be modified during restore"
        );
    }

    // === inode-swap (hardlink retarget) detection ===

    #[test]
    fn hardlink_inode_swap_detected() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let prd = tasks_dir.join("h.json");
        fs::write(&prd, b"{\"v\":1}").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Replace the file with a different file carrying the SAME bytes —
        // content hash unchanged, but inode differs. The guard catches this
        // as a mutation via the (dev, inode) check.
        let replacement = db_dir.join("staging.json");
        fs::write(&replacement, b"{\"v\":1}").unwrap();
        fs::remove_file(&prd).unwrap();
        fs::hard_link(&replacement, &prd).unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::Reverted { restored, .. } => {
                assert_eq!(restored, vec![prd.clone()]);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
    }

    // === new-file detection ===

    #[test]
    fn new_protected_file_is_detected_and_removed() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Codex plants a brand-new JSON file (no prior snapshot of it).
        let planted = tasks_dir.join("evil.json");
        fs::write(&planted, b"{\"injected\":true}").unwrap();
        // Also plant a non-protected file to confirm scope.
        let benign = tasks_dir.join("README.md");
        fs::write(&benign, b"# unrelated").unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::Reverted {
                removed_new_files, ..
            } => {
                assert_eq!(removed_new_files, vec![planted.clone()]);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        assert!(!planted.exists(), "planted file should have been removed");
        assert!(benign.exists(), "non-protected file must NOT be removed");
    }

    #[test]
    fn new_planted_symlink_at_protected_path_is_removed() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);

        // Out-of-tree target the planted symlink would otherwise expose.
        let outside = db_dir.join("evil-target.txt");
        fs::write(&outside, b"SECRET").unwrap();
        let planted = tasks_dir.join("planted.json");
        symlink(&outside, &planted).unwrap();

        match snap.verify_and_restore() {
            VerifyOutcome::Reverted {
                removed_new_files, ..
            } => {
                assert_eq!(removed_new_files, vec![planted.clone()]);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        // Remove must take the LINK, not the target.
        assert!(!planted.exists());
        assert_eq!(fs::read(&outside).unwrap(), b"SECRET");
    }

    // === clean baseline: no mutations ===

    #[test]
    fn unmodified_state_is_clean() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let prd = tasks_dir.join("clean.json");
        fs::write(&prd, b"{\"clean\":true}").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);
        match snap.verify_and_restore() {
            VerifyOutcome::Clean => {}
            other => panic!("expected Clean, got {:?}", other),
        }
        assert_eq!(fs::read(&prd).unwrap(), b"{\"clean\":true}");
    }

    // === scope: non-protected files ignored even on mutation ===

    #[test]
    fn non_protected_file_mutation_is_ignored() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let backlog = tasks_dir.join("backlog.md"); // not -prompt.md
        fs::write(&backlog, b"# initial").unwrap();
        let snap = Snapshot::take_unconditional(&db_dir, &tasks_dir);
        fs::write(&backlog, b"# tampered").unwrap();
        match snap.verify_and_restore() {
            VerifyOutcome::Clean => {}
            other => panic!("expected Clean, got {:?}", other),
        }
        assert_eq!(
            fs::read(&backlog).unwrap(),
            b"# tampered",
            "files outside the protected globs must not be touched"
        );
    }

    // === discriminator: a stub `take` that ignores the gate would route a
    //     Claude run through verify_and_restore. The Codex gate prevents that. ===

    #[test]
    fn discriminator_claude_take_returns_none_so_verify_never_runs() {
        let (_tmp, db_dir, tasks_dir) = make_dirs();
        let prd = tasks_dir.join("a.json");
        fs::write(&prd, b"{\"start\":true}").unwrap();
        // A real Claude/Grok caller never gets a Snapshot, so any mutation
        // they make survives — proving the guard is opt-in by runner.
        let snap = Snapshot::take(&db_dir, &tasks_dir, RunnerKind::Claude);
        assert!(snap.is_none());
        fs::write(&prd, b"{\"claude_wrote_this\":true}").unwrap();
        assert_eq!(fs::read(&prd).unwrap(), b"{\"claude_wrote_this\":true}");
    }

    // === walk_protected: chmod-0000 subdir is skipped without panic ===

    /// When a subdir under tasks_dir has mode 0000 (as Codex might set it),
    /// `path.canonicalize()` returns EACCES. walk_protected must not panic and
    /// must skip the descent (logging a warning). Files planted under the
    /// unreadable dir remain undetected — that is the documented limitation of
    /// this approach; the test validates the graceful-degradation contract.
    ///
    /// Skipped when running as root since root bypasses permission checks.
    #[test]
    fn walk_protected_chmod0000_subdir_skips_without_panic() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses POSIX permission checks — the test would pass trivially
        // and not exercise the error path we're testing.
        if unsafe { libc::getuid() } == 0 {
            return;
        }

        let (_tmp, _db_dir, tasks_dir) = make_dirs();

        let sub = tasks_dir.join("restricted");
        fs::create_dir(&sub).unwrap();
        let prd = sub.join("hidden.json");
        fs::write(&prd, b"{\"secret\":true}").unwrap();

        // chmod 0000 so canonicalize() returns EACCES — this is the scenario
        // where Codex makes a subdir unreadable to hide planted files.
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();

        // Must not panic; the unreadable file is absent from the result.
        let found = current_protected_files(&tasks_dir);
        assert!(
            !found.contains(&prd),
            "file under chmod-0000 subdir must be absent (unreadable)"
        );

        // Restore permissions so TempDir cleanup can remove the directory.
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
