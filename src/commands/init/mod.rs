//! Initialize database from JSON PRD file(s).
//!
//! This module implements the `init` command which imports task data from
//! JSON PRD files into the SQLite database.
//!
//! # Security Considerations
//!
//! ## Trusted vs Untrusted Input
//!
//! - **`--from-json` path (trusted)**: CLI argument from the user running the command.
//!   The user controls which file to import and has filesystem permissions to read it.
//!   No validation is performed on this path.
//!
//! - **`touchesFiles` in PRD (untrusted)**: Paths embedded in the PRD JSON content.
//!   PRD files may come from external sources (shared repos, downloaded files).
//!   These paths are validated to prevent path traversal attacks.
//!
//! ## Path Traversal Protection
//!
//! The `touchesFiles` array in each user story is validated using [`validate_safe_path`].
//! Rejected patterns:
//! - Absolute paths (`/etc/passwd`, `C:\Windows`)
//! - Parent directory traversal (`../../../etc/passwd`)
//! - Home directory expansion (`~/.ssh/id_rsa`)
//! - Network paths (`\\server\share`, `//server/share`)
//!
//! This protects against malicious PRD files that could:
//! - Reference sensitive system files
//! - Escape the project directory
//! - Access network resources
//!
//! [`validate_safe_path`]: crate::error::validate_safe_path

pub mod import;
pub mod output;
pub mod parse;

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::TaskMgrError;
use crate::TaskMgrResult;
use crate::db::lock::{is_named_lock_held, loop_lock_filename};
use crate::db::open_and_migrate as open_connection;
use crate::error::validate_safe_path;
use crate::output::ui;

// Re-export public types
pub use output::{DryRunDeletePreview, InitResult, format_init_verbose, format_text};
pub use parse::{PrdFile, PrdUserStory};

/// Controls how task ID prefixing behaves during import.
#[derive(Debug, Clone)]
pub enum PrefixMode {
    /// Hash-on-first, then sticky path identity.
    ///
    /// On first registration for a path, always uses
    /// `md5(branchName + ":" + filename)[..8]` — JSON `taskPrefix` is **not**
    /// read. That prefix is frozen on the `prd_metadata` row and written back
    /// to the JSON. Later Auto imports for the same path identity restore the
    /// registered prefix (sticky); they do not re-hash or honor hand-edits to
    /// JSON `taskPrefix`.
    Auto,
    /// Use this explicit prefix (`--prefix`). Highest priority over sticky /
    /// Auto hash; mismatch with a registered identity refuses unless `--force`.
    Explicit(String),
    /// No prefix — import task IDs exactly as they appear in the JSON.
    Disabled,
}

impl PrefixMode {
    /// Resolve from the `--no-prefix` / `--prefix <val>` CLI flags.
    pub fn from_cli_flags(no_prefix: bool, prefix: Option<String>) -> Self {
        if no_prefix {
            Self::Disabled
        } else if let Some(p) = prefix {
            Self::Explicit(p)
        } else {
            Self::Auto
        }
    }
}

/// Apply a prefix to a single task ID.
///
/// Idempotent: if the ID already starts with `{prefix}-`, returns it unchanged.
pub(crate) fn prefix_id(prefix: &str, id: &str) -> String {
    let with_dash = format!("{}-", prefix);
    if id.starts_with(&with_dash) {
        id.to_string()
    } else {
        format!("{}-{}", prefix, id)
    }
}

/// Apply a prefix to all IDs and cross-references in a story.
fn prefix_story(prefix: &str, story: &mut PrdUserStory) {
    story.id = prefix_id(prefix, &story.id);
    story.depends_on = story
        .depends_on
        .iter()
        .map(|d| prefix_id(prefix, d))
        .collect();
    story.synergy_with = story
        .synergy_with
        .iter()
        .map(|s| prefix_id(prefix, s))
        .collect();
    story.batch_with = story
        .batch_with
        .iter()
        .map(|b| prefix_id(prefix, b))
        .collect();
    story.conflicts_with = story
        .conflicts_with
        .iter()
        .map(|c| prefix_id(prefix, c))
        .collect();
}

/// Write an auto-generated `taskPrefix` back to the PRD JSON file.
/// Uses `serde_json::Value` to preserve existing formatting and unknown fields.
fn write_prefix_to_json(json_path: &Path, prefix: &str) -> TaskMgrResult<()> {
    let content = std::fs::read_to_string(json_path).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to read {} for prefix write-back: {}",
                json_path.display(),
                e
            ),
        ))
    })?;

    let mut value: serde_json::Value = serde_json::from_str(&content)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "taskPrefix".to_string(),
            serde_json::Value::String(prefix.to_string()),
        );
    }

    let output = serde_json::to_string_pretty(&value)?;
    std::fs::write(json_path, format!("{}\n", output)).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to write prefix to {}: {}", json_path.display(), e),
        ))
    })?;

    Ok(())
}

/// Generate a deterministic 8-char hex prefix from branch name and filename.
///
/// Formula: `md5(branch_name + ":" + filename)[..8]`
///
/// When `branch_name` is `None` or empty, the hash input is `":" + filename`,
/// which is still deterministic per-file.
pub fn generate_prefix(branch_name: Option<&str>, filename: &str) -> String {
    let branch = match branch_name {
        Some(b) if !b.is_empty() => b,
        _ => "",
    };
    let input = format!("{}:{}", branch, filename);
    let digest = md5::compute(input.as_bytes());
    format!("{:x}", digest)[..8].to_string()
}

/// Deterministic Auto prefix for a JSON path. Never reads JSON `taskPrefix`.
fn compute_auto_prefix(json_path: &Path, branch_name: Option<&str>) -> String {
    let filename = json_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unknown.json");
    generate_prefix(branch_name, filename)
}

/// Emit ignore-note and write JSON when Auto hash differs from JSON `taskPrefix`.
fn maybe_write_auto_first_prefix(
    json_path: &Path,
    prd: &PrdFile,
    generated: &str,
    dry_run: bool,
) -> TaskMgrResult<()> {
    if prd.task_prefix.as_deref() != Some(generated) {
        if prd.task_prefix.is_some() {
            ui::emit(&format!(
                "Note: ignoring JSON taskPrefix '{}', using deterministic prefix '{}'",
                prd.task_prefix.as_deref().unwrap_or(""),
                generated,
            ));
        }
        if !dry_run {
            write_prefix_to_json(json_path, generated)?;
        }
    }
    Ok(())
}

/// First-registration prefix selection (no path-identity hit).
///
/// Auto always hashes (`compute_auto_prefix`); JSON `taskPrefix` is never the
/// chosen value. Shared by [`resolve_sticky_prefix`] and scoped `--force`
/// (identity rows already archived → first-reg semantics).
fn first_registration_prefix(
    json_path: &Path,
    prd: &PrdFile,
    prefix_mode: &PrefixMode,
    dry_run: bool,
) -> TaskMgrResult<Option<String>> {
    match prefix_mode {
        PrefixMode::Disabled => Ok(None),
        PrefixMode::Explicit(p) => Ok(Some(p.clone())),
        PrefixMode::Auto => {
            // JSON taskPrefix is NOT read on first Auto (hash-then-freeze).
            let generated = compute_auto_prefix(json_path, prd.branch_name.as_deref());
            maybe_write_auto_first_prefix(json_path, prd, &generated, dry_run)?;
            Ok(Some(generated))
        }
    }
}

/// Prefix after scoped `--force` archived identity rows.
///
/// Prefers the plan's `about_to_apply` so multi-file batches keep the first
/// file's prefix (learning [4601]); falls back to [`first_registration_prefix`].
fn resolve_prefix_after_scoped_force(
    json_path: &Path,
    prd: &PrdFile,
    prefix_mode: &PrefixMode,
    force_about_to_apply: &Option<String>,
    dry_run: bool,
) -> TaskMgrResult<Option<String>> {
    if let Some(pfx) = force_about_to_apply {
        if matches!(prefix_mode, PrefixMode::Auto) {
            maybe_write_auto_first_prefix(json_path, prd, pfx, dry_run)?;
        }
        return Ok(Some(pfx.clone()));
    }
    first_registration_prefix(json_path, prd, prefix_mode, dry_run)
}

/// Refuse when path identity matches 2+ `prd_metadata` rows (twins).
fn refuse_path_identity_twins(
    json_path: &Path,
    hits: &[(i64, Option<String>)],
) -> TaskMgrResult<()> {
    if hits.len() < 2 {
        return Ok(());
    }
    let detail = hits
        .iter()
        .map(|(id, pfx)| match pfx {
            Some(p) => format!("prd_id={id} prefix={p}"),
            None => format!("prd_id={id} prefix=(null)"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(TaskMgrError::InvalidState {
        resource_type: "PRD".to_string(),
        id: json_path.display().to_string(),
        expected: "at most one prd_metadata row for this path identity".to_string(),
        actual: format!(
            "{} path-identity matches ({detail}). Run `task-mgr doctor` to inspect twins before re-init.",
            hits.len()
        ),
    })
}

/// Sticky identity hit: mismatch refuse + optional JSON restore.
fn apply_sticky_registered_prefix(
    json_path: &Path,
    prd: &PrdFile,
    prefix_mode: &PrefixMode,
    prd_id: i64,
    registered_prefix: Option<String>,
    dry_run: bool,
) -> TaskMgrResult<(Option<String>, Option<i64>)> {
    match prefix_mode {
        PrefixMode::Disabled => {
            if let Some(ref rp) = registered_prefix {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "PRD".to_string(),
                    id: json_path.display().to_string(),
                    expected: format!(
                        "registered prefix '{rp}' (use Auto/Explicit, or --force to reprefix)"
                    ),
                    actual: "--no-prefix / PrefixMode::Disabled on a registered non-NULL identity"
                        .to_string(),
                });
            }
        }
        PrefixMode::Explicit(requested) => {
            if registered_prefix.as_deref() != Some(requested.as_str()) {
                let registered_disp = registered_prefix.as_deref().unwrap_or("(null)");
                return Err(TaskMgrError::InvalidState {
                    resource_type: "PRD".to_string(),
                    id: json_path.display().to_string(),
                    expected: format!(
                        "prefix '{registered_disp}' matching registered path identity (or --force)"
                    ),
                    actual: format!("Explicit('{requested}')"),
                });
            }
        }
        PrefixMode::Auto => {
            // Sticky: use registered prefix even when NULL; do not re-hash.
        }
    }

    if let Some(ref rp) = registered_prefix
        && prd.task_prefix.as_deref() != Some(rp.as_str())
        && !dry_run
    {
        write_prefix_to_json(json_path, rp)?;
    }

    Ok((registered_prefix, Some(prd_id)))
}

/// Resolve the task prefix for one JSON file via path identity, then mode.
///
/// Returns `(prefix, sticky_prd_id)`. `sticky_prd_id` is `Some` when the live
/// path already identifies a registered task_list — callers must UPDATE that
/// row and must not INSERT a new `prd_metadata` for a different prefix.
///
/// Order: identity (2+ refuse / 1 sticky / 0 first-reg) → PrefixMode.
/// First `Auto` always hashes; JSON `taskPrefix` is not read on first Auto.
///
/// Shared by `init_with_opts` and loop startup `pre_lock` (FEAT-002b). Startup
/// calls with `dry_run = true` so JSON restore waits until init under the
/// loop lock. Auto loop run on a registered NULL identity is refused by the
/// startup caller, not here (`loop init --no-prefix` first-reg stays allowed).
pub(crate) fn resolve_sticky_prefix(
    conn: &rusqlite::Connection,
    json_path: &Path,
    prd: &PrdFile,
    prefix_mode: &PrefixMode,
    source_root: &Path,
    worktree_root: &Path,
    dry_run: bool,
) -> TaskMgrResult<(Option<String>, Option<i64>)> {
    let hits = find_registered_task_lists(conn, json_path, source_root, worktree_root)?;
    refuse_path_identity_twins(json_path, &hits)?;

    if let Some((prd_id, registered_prefix)) = hits.into_iter().next() {
        return apply_sticky_registered_prefix(
            json_path,
            prd,
            prefix_mode,
            prd_id,
            registered_prefix,
            dry_run,
        );
    }

    let prefix = first_registration_prefix(json_path, prd, prefix_mode, dry_run)?;
    Ok((prefix, None))
}

use import::{
    DEPRECATED_RELATIONSHIPS_WARNING, ForceArchivePlan, delete_task_files,
    delete_task_relationships, drop_existing_data, find_registered_task_lists, force_union_archive,
    get_archive_preview, get_delete_preview, get_existing_task_ids, insert_prd_metadata,
    insert_task, insert_task_file, insert_task_relationships, is_fresh_database,
    register_prd_files, update_prd_metadata_by_id, update_task,
};

/// Build the `--force` archive union: identity prefixes (incl. NULL) ∪ about-to-apply.
///
/// Auto about-to-apply is always the deterministic hash — never JSON `taskPrefix`.
fn build_force_archive_plan(
    conn: &rusqlite::Connection,
    json_path: &Path,
    prefix_mode: &PrefixMode,
    source_root: &Path,
    worktree_root: &Path,
) -> TaskMgrResult<(ForceArchivePlan, Option<String>)> {
    let content = std::fs::read_to_string(json_path).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to read {}: {}", json_path.display(), e),
        ))
    })?;
    let prd: PrdFile = serde_json::from_str(&content)?;

    let hits = find_registered_task_lists(conn, json_path, source_root, worktree_root)?;

    let mut prefixes: BTreeSet<String> = BTreeSet::new();
    let mut null_prd_ids: Vec<i64> = Vec::new();
    let mut includes_null = false;

    for (prd_id, pfx) in hits {
        match pfx {
            Some(p) => {
                prefixes.insert(p);
            }
            None => {
                includes_null = true;
                null_prd_ids.push(prd_id);
            }
        }
    }

    let about_to_apply: Option<String> = match prefix_mode {
        PrefixMode::Disabled => None,
        PrefixMode::Explicit(p) => Some(p.clone()),
        PrefixMode::Auto => Some(compute_auto_prefix(json_path, prd.branch_name.as_deref())),
    };

    if let Some(ref p) = about_to_apply {
        prefixes.insert(p.clone());
    }

    // Auto: also archive JSON taskPrefix when present so a stale label
    // (≠ identity ≠ hash) is covered — known-bad deleted only the JSON name
    // and left the hash metadata row.
    if matches!(prefix_mode, PrefixMode::Auto)
        && let Some(ref jp) = prd.task_prefix
    {
        prefixes.insert(jp.clone());
    }

    // Disabled + NULL identity: includes_null already set from hits.
    // Disabled + no identity: plan empty → legacy global wipe.

    Ok((
        ForceArchivePlan {
            prefixes,
            null_prd_ids,
            includes_null,
        },
        about_to_apply,
    ))
}

/// Refuse `--force` when any union member's loop lock is held.
fn refuse_if_force_locks_held(db_dir: &Path, plan: &ForceArchivePlan) -> TaskMgrResult<()> {
    let mut to_check: Vec<Option<&str>> = plan.prefixes.iter().map(|p| Some(p.as_str())).collect();
    if plan.includes_null {
        to_check.push(None);
    }
    for prefix in to_check {
        let filename = loop_lock_filename(prefix);
        if is_named_lock_held(db_dir, &filename) {
            return Err(TaskMgrError::InvalidState {
                resource_type: "loop lock".to_string(),
                id: filename.clone(),
                expected: format!(
                    "no active loop lock for --force union member {}",
                    prefix.unwrap_or("(null)")
                ),
                actual: format!("{filename} is held; refuse --force (no partial archive)"),
            });
        }
    }
    Ok(())
}

/// Optional roots for PRD path identity and `prd_files` storage.
///
/// Production loop/batch callers **must** pass explicit `source_root` and
/// `worktree_root` (loop `source_root` ≠ `db_dir`). [`Default`] leaves both
/// `None` so the ~90 test call sites of the 7-arg [`init`] wrapper keep
/// compiling; unresolved roots fall back to `main_repo_root_at(db_dir)` then
/// the parent of `db_dir` (project root when `db_dir` is `.task-mgr`).
#[derive(Debug, Clone, Default)]
pub struct InitOpts {
    /// Project / main-checkout root used to store source-root-relative paths.
    pub source_root: Option<PathBuf>,
    /// Live worktree root for pin-19 remap on read (often equals `source_root`).
    pub worktree_root: Option<PathBuf>,
}

impl InitOpts {
    /// Resolve concrete roots for this init invocation.
    pub fn resolve_roots(&self, db_dir: &Path) -> (PathBuf, PathBuf) {
        let source_root = self.source_root.clone().unwrap_or_else(|| {
            crate::git::main_repo_root_at(db_dir)
                .or_else(|| db_dir.parent().map(|p| p.to_path_buf()))
                .unwrap_or_else(|| db_dir.to_path_buf())
        });
        let worktree_root = self
            .worktree_root
            .clone()
            .unwrap_or_else(|| source_root.clone());
        (source_root, worktree_root)
    }
}

/// Initialize the database from JSON PRD file(s).
///
/// Thin wrapper around [`init_with_opts`] with [`InitOpts::default`]. Prefer
/// [`init_with_opts`] in production so `prd_files` storage uses the real
/// project `source_root` (not a Default fallback derived from `db_dir`).
///
/// # Arguments
///
/// * `dir` - Directory for database files
/// * `json_files` - Path(s) to JSON PRD file(s)
/// * `force` - If true, drop existing data before import
/// * `append` - If true, add to existing data (for multi-phase projects)
/// * `update_existing` - If true with append, update existing tasks
/// * `dry_run` - If true, preview changes without making them
/// * `prefix_mode` - Controls task ID prefixing behavior
///
/// # Returns
///
/// Returns an `InitResult` with import statistics.
///
/// # Errors
///
/// Returns an error if:
/// - Any JSON file cannot be read or parsed
/// - Database operations fail
/// - Duplicate task IDs are found across files (when not in append mode)
/// - Cross-file dependencies reference non-existent tasks
pub fn init(
    dir: &Path,
    json_files: &[impl AsRef<Path>],
    force: bool,
    append: bool,
    update_existing: bool,
    dry_run: bool,
    prefix_mode: PrefixMode,
) -> TaskMgrResult<InitResult> {
    init_with_opts(
        dir,
        json_files,
        force,
        append,
        update_existing,
        dry_run,
        prefix_mode,
        InitOpts::default(),
    )
}

/// Initialize the database from JSON PRD file(s), with explicit project roots.
///
/// Same behavior as [`init`], but `opts.source_root` / `opts.worktree_root`
/// drive `prd_files` storage (source-root-relative POSIX) and identity math.
///
/// The 7 positional flags match [`init`]; `opts` is intentionally a trailing
/// struct rather than more positionals so test call sites stay on the wrapper.
#[allow(clippy::too_many_arguments)]
pub fn init_with_opts(
    dir: &Path,
    json_files: &[impl AsRef<Path>],
    force: bool,
    append: bool,
    update_existing: bool,
    dry_run: bool,
    prefix_mode: PrefixMode,
    opts: InitOpts,
) -> TaskMgrResult<InitResult> {
    let (source_root, worktree_root) = opts.resolve_roots(dir);
    let mut conn = open_connection(dir)?;
    crate::db::run_migrations(&mut conn)?;

    let force_prep = prepare_force_phase(
        &conn,
        dir,
        json_files,
        force,
        dry_run,
        &prefix_mode,
        &source_root,
        &worktree_root,
    )?;

    let fresh_import = if dry_run && force {
        true
    } else {
        is_fresh_database(&conn)?
    };

    // Append reconcile OR prefix-scoped --force unarchive/update. Soft-archive
    // leaves physical rows; INSERT would UNIQUE-crash without this. The
    // `fresh_import` gate is intentionally NOT applied: archived-only DBs
    // report fresh but still have conflicting rows.
    let reconcile_existing = append || (force && force_prep.use_scoped);
    let existing_ids = if reconcile_existing {
        get_existing_task_ids(&conn)?
    } else {
        HashSet::new()
    };

    let collected = collect_import_from_files(
        &conn,
        json_files,
        &prefix_mode,
        &source_root,
        &worktree_root,
        dry_run,
        force,
        update_existing,
        reconcile_existing,
        &existing_ids,
        force_prep.use_scoped,
        &force_prep.about_to_apply,
    )?;

    validate_collected_stories(&collected)?;

    let (tasks_imported, tasks_updated, files_imported, relationships_imported) =
        count_import_stats(&collected);

    if dry_run {
        return Ok(InitResult {
            tasks_imported,
            tasks_updated,
            tasks_skipped: collected.tasks_skipped,
            files_imported,
            relationships_imported,
            fresh_import,
            warnings: collected.warnings,
            dry_run: true,
            would_delete: force_prep.would_delete,
            would_archive: force_prep.would_archive,
            prefix_applied: collected.resolved_prefix,
            created_dirs: false,
            created_config: false,
        });
    }

    commit_import(&mut conn, &collected, force, &source_root)?;

    // Note: the model picker is intentionally NOT invoked here. Per the
    // Init-split contract (FEAT-005), project-scoped picker firing lives in
    // [`init_project`]. The deprecated `task-mgr init --from-json X` shim
    // calls `init_project` before dispatching here, so the picker still
    // fires for that path. Direct `task-mgr loop init` / `task-mgr batch
    // init` invocations are PRD-scoped and must not fire the picker.

    Ok(InitResult {
        tasks_imported,
        tasks_updated,
        tasks_skipped: collected.tasks_skipped,
        files_imported,
        relationships_imported,
        fresh_import,
        warnings: collected.warnings,
        dry_run: false,
        would_delete: None,
        would_archive: None,
        prefix_applied: collected.resolved_prefix,
        created_dirs: false,
        created_config: false,
    })
}

/// Outcome of the `--force` pre-import phase (plan, locks, archive/wipe, preview).
struct ForcePrep {
    use_scoped: bool,
    about_to_apply: Option<String>,
    would_delete: Option<DryRunDeletePreview>,
    would_archive: Option<output::DryRunArchivePreview>,
}

/// Build force plan, refuse held locks, preview or soft-archive/wipe.
#[allow(clippy::too_many_arguments)]
fn prepare_force_phase(
    conn: &rusqlite::Connection,
    dir: &Path,
    json_files: &[impl AsRef<Path>],
    force: bool,
    dry_run: bool,
    prefix_mode: &PrefixMode,
    source_root: &Path,
    worktree_root: &Path,
) -> TaskMgrResult<ForcePrep> {
    if !force {
        return Ok(ForcePrep {
            use_scoped: false,
            about_to_apply: None,
            would_delete: None,
            would_archive: None,
        });
    }

    // Prefix-scoped --force: soft-archive identity ∪ about-to-apply (FEAT-003).
    // Twins contribute every identity prefix (no LIMIT 1). Empty plan + Disabled
    // falls through to legacy global wipe via drop_existing_data(None).
    let first = json_files
        .first()
        .ok_or_else(|| TaskMgrError::InvalidState {
            resource_type: "PRD".to_string(),
            id: "init --force".to_string(),
            expected: "at least one JSON file".to_string(),
            actual: "empty json_files".to_string(),
        })?;
    let (plan, about_to_apply) = build_force_archive_plan(
        conn,
        first.as_ref(),
        prefix_mode,
        source_root,
        worktree_root,
    )?;
    let use_scoped = plan.is_scoped();

    if use_scoped {
        refuse_if_force_locks_held(dir, &plan)?;
    }

    let (would_delete, would_archive) = if dry_run {
        if use_scoped {
            (None, Some(get_archive_preview(conn, &plan)?))
        } else {
            (Some(get_delete_preview(conn, None)?), None)
        }
    } else {
        (None, None)
    };

    if !dry_run {
        if use_scoped {
            force_union_archive(conn, &plan)?;
        } else {
            drop_existing_data(conn, None)?;
        }
    }

    Ok(ForcePrep {
        use_scoped,
        about_to_apply,
        would_delete,
        would_archive,
    })
}

/// Parsed stories + metadata gathered from all JSON inputs before DB write.
struct CollectedImport {
    all_stories: Vec<PrdUserStory>,
    stories_to_update: Vec<PrdUserStory>,
    prd_metadata: Option<PrdFile>,
    raw_json: Option<String>,
    warnings: Vec<String>,
    tasks_skipped: usize,
    resolved_prefix: Option<String>,
    json_file_registrations: Vec<(PathBuf, PrdFile)>,
    sticky_prd_id: Option<i64>,
    all_task_ids: HashSet<String>,
}

/// Parse JSON files, resolve prefixes, and partition new vs update stories.
#[allow(clippy::too_many_arguments)]
fn collect_import_from_files(
    conn: &rusqlite::Connection,
    json_files: &[impl AsRef<Path>],
    prefix_mode: &PrefixMode,
    source_root: &Path,
    worktree_root: &Path,
    dry_run: bool,
    force: bool,
    update_existing: bool,
    reconcile_existing: bool,
    existing_ids: &HashSet<String>,
    use_scoped_force: bool,
    force_about_to_apply: &Option<String>,
) -> TaskMgrResult<CollectedImport> {
    let mut collected = CollectedImport {
        all_stories: Vec::new(),
        stories_to_update: Vec::new(),
        prd_metadata: None,
        raw_json: None,
        warnings: Vec::new(),
        tasks_skipped: 0,
        resolved_prefix: None,
        json_file_registrations: Vec::new(),
        sticky_prd_id: None,
        all_task_ids: existing_ids.clone(),
    };

    for (file_idx, json_path) in json_files.iter().enumerate() {
        let json_path = json_path.as_ref();
        let content = std::fs::read_to_string(json_path).map_err(|e| {
            TaskMgrError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to read {}: {}", json_path.display(), e),
            ))
        })?;
        let prd: PrdFile = serde_json::from_str(&content)?;

        let (file_prefix, file_sticky_prd_id) = resolve_file_import_prefix(
            conn,
            json_path,
            &prd,
            prefix_mode,
            source_root,
            worktree_root,
            dry_run,
            use_scoped_force,
            force_about_to_apply,
        )?;

        refuse_mixed_batch_identity(
            json_path,
            file_idx,
            file_prefix.as_ref(),
            file_sticky_prd_id,
            &mut collected,
        )?;

        if file_idx == 0 {
            collected.resolved_prefix = file_prefix.clone();
        }

        accumulate_stories_for_file(
            &prd,
            file_prefix.as_ref(),
            reconcile_existing,
            existing_ids,
            force,
            update_existing,
            &mut collected,
        )?;

        if collected.prd_metadata.is_none() {
            collected.raw_json = Some(content);
            collected.prd_metadata = Some(metadata_shell_from_prd(&prd, file_prefix.clone()));
        }

        collected.json_file_registrations.push((
            json_path.to_path_buf(),
            registration_shell_from_prd(&prd, file_prefix.clone()),
        ));

        collect_new_stories(
            prd,
            file_prefix.as_ref(),
            reconcile_existing,
            existing_ids,
            &mut collected,
        );
    }

    Ok(collected)
}

/// Sticky identity or first-reg / post-force prefix for one JSON file.
#[allow(clippy::too_many_arguments)]
fn resolve_file_import_prefix(
    conn: &rusqlite::Connection,
    json_path: &Path,
    prd: &PrdFile,
    prefix_mode: &PrefixMode,
    source_root: &Path,
    worktree_root: &Path,
    dry_run: bool,
    use_scoped_force: bool,
    force_about_to_apply: &Option<String>,
) -> TaskMgrResult<(Option<String>, Option<i64>)> {
    // After scoped --force (or dry-run simulating it), identity rows are gone —
    // use about-to-apply / first-reg rather than sticky (avoids twin refuse on
    // dry-run and wrong prefix_applied preview).
    if use_scoped_force {
        let pfx = resolve_prefix_after_scoped_force(
            json_path,
            prd,
            prefix_mode,
            force_about_to_apply,
            dry_run,
        )?;
        return Ok((pfx, None));
    }
    resolve_sticky_prefix(
        conn,
        json_path,
        prd,
        prefix_mode,
        source_root,
        worktree_root,
        dry_run,
    )
}

/// Refuse attaching files that resolve to different identity prefixes/prd_ids
/// in one init() batch (learning [4601]).
fn refuse_mixed_batch_identity(
    json_path: &Path,
    file_idx: usize,
    file_prefix: Option<&String>,
    file_sticky_prd_id: Option<i64>,
    collected: &mut CollectedImport,
) -> TaskMgrResult<()> {
    let file_prefix_owned = file_prefix.cloned();
    if let Some(sid) = file_sticky_prd_id {
        match collected.sticky_prd_id {
            None => collected.sticky_prd_id = Some(sid),
            Some(existing) if existing == sid => {}
            Some(existing) => {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "PRD".to_string(),
                    id: json_path.display().to_string(),
                    expected: format!(
                        "all files in one init() to share path-identity prd_id {existing}"
                    ),
                    actual: format!(
                        "file resolves to prd_id {sid} (prefix {:?})",
                        file_prefix_owned
                    ),
                });
            }
        }
        if file_idx > 0 && collected.resolved_prefix != file_prefix_owned {
            return Err(TaskMgrError::InvalidState {
                resource_type: "PRD".to_string(),
                id: json_path.display().to_string(),
                expected: format!(
                    "prefix {:?} matching earlier files in this init()",
                    collected.resolved_prefix
                ),
                actual: format!("sticky prefix {:?}", file_prefix_owned),
            });
        }
    } else if collected.sticky_prd_id.is_some()
        && file_idx > 0
        && collected.resolved_prefix != file_prefix_owned
    {
        return Err(TaskMgrError::InvalidState {
            resource_type: "PRD".to_string(),
            id: json_path.display().to_string(),
            expected: format!(
                "prefix {:?} matching sticky identity of earlier files",
                collected.resolved_prefix
            ),
            actual: format!("first-registration prefix {:?}", file_prefix_owned),
        });
    }
    Ok(())
}

/// First-pass: track IDs, queue updates, detect in-batch duplicates.
fn accumulate_stories_for_file(
    prd: &PrdFile,
    file_prefix: Option<&String>,
    reconcile_existing: bool,
    existing_ids: &HashSet<String>,
    force: bool,
    update_existing: bool,
    collected: &mut CollectedImport,
) -> TaskMgrResult<()> {
    for story in &prd.user_stories {
        let effective_id = if let Some(pfx) = file_prefix {
            prefix_id(pfx, &story.id)
        } else {
            story.id.clone()
        };
        collected.all_task_ids.insert(effective_id.clone());

        if reconcile_existing && existing_ids.contains(&effective_id) {
            if force || update_existing {
                let mut s = story.clone();
                if let Some(pfx) = file_prefix {
                    prefix_story(pfx, &mut s);
                }
                collected.stories_to_update.push(s);
            } else {
                collected
                    .warnings
                    .push(format!("Skipping existing task: {}", effective_id));
                collected.tasks_skipped += 1;
            }
            continue;
        }

        let already_imported = collected.all_stories.iter().any(|s| s.id == effective_id);
        if already_imported {
            return Err(TaskMgrError::InvalidState {
                resource_type: "Task".to_string(),
                id: effective_id,
                expected: "Unique task IDs across all files".to_string(),
                actual: "Duplicate ID found in multiple files".to_string(),
            });
        }
    }
    Ok(())
}

fn metadata_shell_from_prd(prd: &PrdFile, file_prefix: Option<String>) -> PrdFile {
    PrdFile {
        project: prd.project.clone(),
        branch_name: prd.branch_name.clone(),
        description: prd.description.clone(),
        priority_philosophy: prd.priority_philosophy.clone(),
        global_acceptance_criteria: prd.global_acceptance_criteria.clone(),
        review_guidelines: prd.review_guidelines.clone(),
        user_stories: Vec::new(),
        external_git_repo: prd.external_git_repo.clone(),
        task_prefix: file_prefix.or_else(|| prd.task_prefix.clone()),
        prd_file: prd.prd_file.clone(),
        model: prd.model.clone(),
        default_max_retries: prd.default_max_retries,
        implicit_overlap_files: prd.implicit_overlap_files.clone(),
    }
}

fn registration_shell_from_prd(prd: &PrdFile, file_prefix: Option<String>) -> PrdFile {
    PrdFile {
        project: prd.project.clone(),
        branch_name: None,
        description: None,
        priority_philosophy: None,
        global_acceptance_criteria: None,
        review_guidelines: None,
        user_stories: Vec::new(),
        external_git_repo: None,
        task_prefix: file_prefix,
        prd_file: prd.prd_file.clone(),
        model: None,
        default_max_retries: None,
        implicit_overlap_files: None,
    }
}

/// Second-pass: push non-existing stories into `all_stories` with prefix applied.
fn collect_new_stories(
    prd: PrdFile,
    file_prefix: Option<&String>,
    reconcile_existing: bool,
    existing_ids: &HashSet<String>,
    collected: &mut CollectedImport,
) {
    for story in prd.user_stories {
        let effective_id = if let Some(pfx) = file_prefix {
            prefix_id(pfx, &story.id)
        } else {
            story.id.clone()
        };

        if reconcile_existing && existing_ids.contains(&effective_id) {
            continue;
        }

        let mut s = story;
        if let Some(pfx) = file_prefix {
            prefix_story(pfx, &mut s);
        }
        collected.all_stories.push(s);
    }
}

fn validate_collected_stories(collected: &CollectedImport) -> TaskMgrResult<()> {
    for story in collected
        .all_stories
        .iter()
        .chain(collected.stories_to_update.iter())
    {
        for dep in &story.depends_on {
            if !collected.all_task_ids.contains(dep) {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "Task".to_string(),
                    id: story.id.clone(),
                    expected: format!("dependsOn task '{}' to exist", dep),
                    actual: "Referenced task not found in any input file or database".to_string(),
                });
            }
        }
    }

    for story in collected
        .all_stories
        .iter()
        .chain(collected.stories_to_update.iter())
    {
        for file_path in &story.touches_files {
            validate_safe_path(file_path, "touchesFiles", Some(&story.id))?;
        }
    }
    Ok(())
}

fn count_import_stats(collected: &CollectedImport) -> (usize, usize, usize, usize) {
    let mut tasks_imported = 0;
    let mut tasks_updated = 0;
    let mut files_imported = 0;
    let mut relationships_imported = 0;

    for story in &collected.all_stories {
        tasks_imported += 1;
        files_imported += story.touches_files.len();
        relationships_imported += story.depends_on.len();
    }
    for story in &collected.stories_to_update {
        tasks_updated += 1;
        files_imported += story.touches_files.len();
        relationships_imported += story.depends_on.len();
    }
    (
        tasks_imported,
        tasks_updated,
        files_imported,
        relationships_imported,
    )
}

/// Insert/update tasks, files, relationships, and prd_metadata inside one tx.
fn commit_import(
    conn: &mut rusqlite::Connection,
    collected: &CollectedImport,
    force: bool,
    source_root: &Path,
) -> TaskMgrResult<()> {
    let tx = conn.transaction()?;
    let prd_default_max_retries = collected
        .prd_metadata
        .as_ref()
        .and_then(|m| m.default_max_retries);

    // Known path identity → UPDATE by prd_id (never INSERT a new prefix row).
    // First registration → insert_prd_metadata (ON CONFLICT task_prefix only).
    let prd_id = if let Some(sid) = collected.sticky_prd_id {
        if let Some(ref metadata) = collected.prd_metadata {
            update_prd_metadata_by_id(&tx, sid, metadata, collected.raw_json.as_deref())?;
        }
        sid
    } else if let Some(ref metadata) = collected.prd_metadata {
        insert_prd_metadata(&tx, metadata, collected.raw_json.as_deref())?
    } else {
        1 // fallback: no metadata parsed (shouldn't happen in practice)
    };

    for (json_path, prd_for_reg) in &collected.json_file_registrations {
        register_prd_files(&tx, prd_id, json_path, prd_for_reg, source_root)?;
    }

    let mut saw_deprecated_relationships = false;

    for story in &collected.all_stories {
        insert_task(&tx, story, prd_default_max_retries)?;
        for file_path in &story.touches_files {
            insert_task_file(&tx, &story.id, file_path)?;
        }
        let outcome = insert_task_relationships(&tx, story)?;
        saw_deprecated_relationships |= outcome.had_deprecated;
    }

    for story in &collected.stories_to_update {
        update_task(&tx, story, prd_default_max_retries)?;
        apply_update_status(&tx, story, force)?;
        delete_task_files(&tx, &story.id)?;
        delete_task_relationships(&tx, &story.id)?;
        for file_path in &story.touches_files {
            insert_task_file(&tx, &story.id, file_path)?;
        }
        let outcome = insert_task_relationships(&tx, story)?;
        saw_deprecated_relationships |= outcome.had_deprecated;
    }

    tx.commit()?;

    if saw_deprecated_relationships {
        ui::emit(DEPRECATED_RELATIONSHIPS_WARNING);
    }
    Ok(())
}

/// Status transitions for `--update-existing` / `--force` unarchive rows.
fn apply_update_status(
    tx: &rusqlite::Connection,
    story: &PrdUserStory,
    force: bool,
) -> TaskMgrResult<()> {
    // Single status-write site (lifecycle_exception_lint: exactly one
    // LIFECYCLE-EXCEPTION token outside src/lifecycle/).
    let new_status: Option<&str> = if force {
        // Force re-import: revive archived rows to todo (or done when passes).
        Some(if story.passes { "done" } else { "todo" })
    } else if story.passes {
        let current_status: String = tx
            .query_row(
                "SELECT status FROM tasks WHERE id = ?",
                [&story.id],
                |row| row.get(0),
            )
            .unwrap_or_default();
        if current_status != "done" {
            Some("done")
        } else {
            None
        }
    } else {
        None
    };

    if let Some(status) = new_status {
        // LIFECYCLE-EXCEPTION: bootstrap ingest — see tasks/prd-tasklifecycle-extraction.md and docs/designs/coherence-refactoring.md §"TaskLifecycle Scope Decision"
        tx.execute(
            "UPDATE tasks SET status = ?, updated_at = datetime('now') WHERE id = ?",
            rusqlite::params![status, &story.id],
        )?;
    }
    Ok(())
}

/// Write `version: 1` default into `<db_dir>/config.json`, preserving any
/// existing fields. Atomic via tempfile + rename; never clobbers unknown keys.
///
/// Returns `true` when the file was newly created, `false` when it already existed.
fn write_project_defaults(db_dir: &Path) -> std::io::Result<bool> {
    use std::io::Write;

    let path = db_dir.join("config.json");
    let created = !path.exists();

    let mut value: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => serde_json::json!({}),
    };

    let obj = value.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config.json is not a JSON object",
        )
    })?;

    if !obj.contains_key("version") {
        obj.insert("version".to_string(), serde_json::json!(1));
    }

    let contents = serde_json::to_string_pretty(&value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let dir = path.parent().unwrap_or(db_dir);
    let mut tmp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".json")
        .tempfile_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.write_all(b"\n")?;
    tmp.persist(&path).map_err(|e| e.error)?;

    Ok(created)
}

/// Marker block delimiting task-mgr's managed entries in `.gitignore`.
/// The block is rewritten in place when its contents would change; any
/// user-authored lines outside the markers are preserved untouched.
const GITIGNORE_MARKER_BEGIN: &str = "# task-mgr begin: progress files (untracked)";
const GITIGNORE_MARKER_END: &str = "# task-mgr end: progress files (untracked)";

/// The patterns inserted between the gitignore markers: per-PRD progress files
/// and the diagnostics log directory (CONTRACT-LOG-001 channel B), both
/// task-mgr-managed and untracked.
const GITIGNORE_BODY: &str = "tasks/progress-*.txt\n.task-mgr/logs/\n";

/// Compute the new contents of `.gitignore` after ensuring the task-mgr managed
/// block is present and has the expected body. Returns `None` when no rewrite is
/// needed (block already matches).
fn merged_gitignore_contents(existing: &str) -> Option<String> {
    crate::util::marker_splice::merge_marker_block(
        existing,
        GITIGNORE_MARKER_BEGIN,
        GITIGNORE_MARKER_END,
        GITIGNORE_BODY,
    )
}

/// Ensure `.gitignore` at `project_root` contains a marker-delimited block that
/// ignores `tasks/progress-*.txt`. Idempotent: only rewrites when the managed
/// block is missing or its body has drifted. Failures are returned as `Err` so
/// the caller can decide whether to propagate or warn.
fn ensure_progress_gitignore(project_root: &Path) -> Result<(), String> {
    let gitignore_path = project_root.join(".gitignore");
    let existing = match std::fs::read_to_string(&gitignore_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {}", gitignore_path.display(), e)),
    };
    if let Some(updated) = merged_gitignore_contents(&existing) {
        std::fs::write(&gitignore_path, updated)
            .map_err(|e| format!("write {}: {}", gitignore_path.display(), e))?;
    }
    Ok(())
}

/// Untrack any `tasks/progress-*.txt` files currently tracked by git using
/// `git rm --cached` (preserving disk content), then commit the index change.
///
/// No-op (`Ok(())`) when there are no tracked progress files, when the
/// directory is not a git repository (`git ls-files` non-zero exit → stderr
/// warning), or when the operator already has staged changes — the migration
/// commits the whole index, so it refuses to run rather than sweep unrelated
/// in-flight work into the `chore: untrack` commit. `init` is idempotent, so
/// the migration simply runs on a later invocation once the index is clean.
///
/// Returns `Err` if `git rm --cached` or `git commit` fail in an actual repo;
/// the caller treats that as a non-fatal warning so `init` still succeeds.
/// `--no-verify` skips hooks because this is a mechanical housekeeping commit.
/// On any failure *after* paths were staged, the staged removals are rolled
/// back (`git reset`) so the operator is never left with a half-migrated index.
fn untrack_progress_files(project_root: &Path) -> Result<(), String> {
    use std::process::Command;

    let ls_output = match Command::new("git")
        .args(["ls-files", "tasks/progress-*.txt"])
        .current_dir(project_root)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            // Not a fatal condition: task-mgr is usable outside a git repo.
            ui::emit(&format!(
                "task-mgr: progress migration skipped (git unavailable: {e})"
            ));
            return Ok(());
        }
    };

    if !ls_output.status.success() {
        ui::emit(&format!(
            "task-mgr: progress migration skipped ({})",
            String::from_utf8_lossy(&ls_output.stderr).trim()
        ));
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&ls_output.stdout);
    let files: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    if files.is_empty() {
        return Ok(());
    }

    // Refuse to run when the operator already has staged changes: the migration
    // commits the whole index, so proceeding would sweep their in-flight work
    // into the `chore: untrack` commit. `git diff --cached --quiet` exits 0 when
    // the index is clean, non-zero when something is staged.
    match Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(project_root)
        .output()
    {
        Ok(o) if !o.status.success() => {
            ui::emit(
                "task-mgr: progress migration skipped — you have staged changes; \
                 commit or unstage them and re-run `task-mgr init`",
            );
            return Ok(());
        }
        Ok(_) => {}
        Err(e) => return Err(format!("git diff --cached spawn failed: {}", e)),
    }

    for file in &files {
        let rm_output = Command::new("git")
            .args(["rm", "--cached", file])
            .current_dir(project_root)
            .output()
            .map_err(|e| format!("git rm --cached '{}' spawn failed: {}", file, e))?;
        if !rm_output.status.success() {
            unstage_paths(project_root, &files);
            return Err(format!(
                "git rm --cached '{}' failed: {}",
                file,
                String::from_utf8_lossy(&rm_output.stderr).trim()
            ));
        }
    }

    // The index now holds only our staged removals (verified clean above), so a
    // plain commit captures exactly the migration and nothing else.
    let commit_output = Command::new("git")
        .args([
            "commit",
            "-m",
            "chore: untrack progress files (task-mgr init migration)",
            "--no-verify",
        ])
        .current_dir(project_root)
        .output();

    match commit_output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            unstage_paths(project_root, &files);
            Err(format!(
                "git commit failed after git rm --cached (staged removals rolled back): {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => {
            unstage_paths(project_root, &files);
            Err(format!("git commit spawn failed: {}", e))
        }
    }
}

/// Best-effort `git reset -- <pathspec>` to undo staged progress-file removals
/// when the migration cannot complete. Restores the index entries for those
/// paths from HEAD so a failed migration leaves no half-staged state. Failures
/// here are only logged — there is no further recovery to attempt.
fn unstage_paths(project_root: &Path, files: &[&str]) {
    use std::process::Command;

    let mut args: Vec<&str> = vec!["reset", "--quiet", "HEAD", "--"];
    args.extend(files.iter().copied());
    if let Err(e) = Command::new("git")
        .args(&args)
        .current_dir(project_root)
        .output()
    {
        ui::emit_err(&format!(
            "task-mgr: warning: could not roll back staged progress removals: {e}"
        ));
    }
}

/// Initialize project-level scaffolding: `.task-mgr/` directory, SQLite DB with
/// migrations, and a default `config.json` (preserving any existing fields).
/// Also ensures `.gitignore` contains an entry for `tasks/progress-*.txt` and
/// runs a one-time migration to untrack any already-tracked progress files.
///
/// Idempotent: safe to call when `.task-mgr/` already exists. Does NOT read or
/// write any PRD JSON file.
///
/// # Arguments
///
/// * `dir` - Project root directory; `.task-mgr/` is created inside it.
pub fn init_project(dir: &Path) -> TaskMgrResult<InitResult> {
    let db_dir = dir.join(".task-mgr");

    let created_dirs = !db_dir.exists();

    // Create .task-mgr/, tasks.db, and run migrations (all idempotent).
    let _ = open_connection(&db_dir)?;

    // Write default config, preserving any existing fields.
    // FR-002 hard break: no legacy-config auto-migration on init — legacy model
    // keys hard-error at the loop/batch preflight (operators migrate explicitly
    // via `models init --force-replace-legacy`, FEAT-009).
    let created_config = write_project_defaults(&db_dir).map_err(TaskMgrError::IoError)?;

    // Fire the anchor-tier picker only when stdin+stderr are both TTYs and no
    // models config exists yet; skip silently (with a hint) otherwise (FR-009).
    let _ = crate::commands::models::ensure_default::ensure_models_anchor(&db_dir, false);

    // Ensure .gitignore ignores per-PRD progress files. Non-fatal: a warning is
    // printed if the file can't be written, but init_project still succeeds.
    if let Err(e) = ensure_progress_gitignore(dir) {
        ui::emit_err(&format!(
            "task-mgr: warning: could not update .gitignore: {e}"
        ));
    }

    // One-time migration: untrack any already-tracked progress files. Fully
    // non-fatal — any failure (not a git repo, no git identity, hook rejection)
    // is reported as a warning and any staged removals are rolled back inside
    // `untrack_progress_files`, so `init` still succeeds with a clean index.
    if let Err(e) = untrack_progress_files(dir) {
        ui::emit_err(&format!(
            "task-mgr: warning: progress migration failed: {e}"
        ));
    }

    Ok(InitResult {
        tasks_imported: 0,
        tasks_updated: 0,
        tasks_skipped: 0,
        files_imported: 0,
        relationships_imported: 0,
        fresh_import: created_dirs,
        warnings: Vec::new(),
        dry_run: false,
        would_delete: None,
        would_archive: None,
        prefix_applied: None,
        created_dirs,
        created_config,
    })
}
