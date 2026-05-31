use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::runner::RunnerKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectedKind {
    RestorableText,
    FatalSqlite,
}

#[derive(Debug, Clone)]
struct Entry {
    kind: ProtectedKind,
    bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProtectedTaskStateSnapshot {
    db_dir: PathBuf,
    entries: BTreeMap<PathBuf, Entry>,
}

impl ProtectedTaskStateSnapshot {
    pub(crate) fn capture(db_dir: &Path) -> TaskMgrResult<Self> {
        let mut entries = BTreeMap::new();
        let tasks_dir = db_dir.join("tasks");
        collect_text_candidates(&tasks_dir, &mut entries)?;
        for sqlite in ["tasks.db", "tasks.db-wal", "tasks.db-shm"] {
            let path = db_dir.join(sqlite);
            entries.insert(
                path.clone(),
                Entry {
                    kind: ProtectedKind::FatalSqlite,
                    bytes: read_no_follow_optional(&path)?,
                },
            );
        }
        Ok(Self {
            db_dir: db_dir.to_path_buf(),
            entries,
        })
    }

    pub(crate) fn verify_and_restore_text(&self) -> TaskMgrResult<()> {
        let mut after = self.entries.clone();
        collect_text_candidates(&self.db_dir.join("tasks"), &mut after)?;
        let all_paths: BTreeSet<PathBuf> =
            self.entries.keys().chain(after.keys()).cloned().collect();

        let mut text_mutations = Vec::new();
        let mut fatal_mutations = Vec::new();
        for path in all_paths {
            let before = self.entries.get(&path);
            let kind = before
                .map(|e| e.kind)
                .or_else(|| after.get(&path).map(|e| e.kind))
                .unwrap_or(ProtectedKind::RestorableText);
            let current = read_no_follow_optional(&path)?;
            let previous = before.and_then(|e| e.bytes.as_ref());
            if previous.map(Vec::as_slice) != current.as_deref() {
                match kind {
                    ProtectedKind::RestorableText => text_mutations.push(path),
                    ProtectedKind::FatalSqlite => fatal_mutations.push(path),
                }
            }
        }

        if !fatal_mutations.is_empty() {
            return Err(TaskMgrError::ProtectedTaskStateMutation {
                runner_kind: RunnerKind::Codex,
                paths: fatal_mutations
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                fatal: true,
            });
        }

        for path in &text_mutations {
            let entry = self.entries.get(path);
            match entry.and_then(|e| e.bytes.as_ref()) {
                Some(bytes) => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, bytes)?;
                }
                None => {
                    if path.exists() {
                        fs::remove_file(path)?;
                    }
                }
            }
        }

        if !text_mutations.is_empty() {
            return Err(TaskMgrError::ProtectedTaskStateMutation {
                runner_kind: RunnerKind::Codex,
                paths: text_mutations
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                fatal: false,
            });
        }
        Ok(())
    }
}

fn collect_text_candidates(dir: &Path, out: &mut BTreeMap<PathBuf, Entry>) -> TaskMgrResult<()> {
    let last_branch = dir.join(".last-branch");
    out.insert(
        last_branch.clone(),
        Entry {
            kind: ProtectedKind::RestorableText,
            bytes: read_no_follow_optional(&last_branch)?,
        },
    );
    if !dir.exists() {
        return Ok(());
    }
    collect_text_candidates_inner(dir, out)
}

fn collect_text_candidates_inner(
    dir: &Path,
    out: &mut BTreeMap<PathBuf, Entry>,
) -> TaskMgrResult<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            out.insert(
                path,
                Entry {
                    kind: ProtectedKind::RestorableText,
                    bytes: None,
                },
            );
            continue;
        }
        if meta.is_dir() {
            collect_text_candidates_inner(&path, out)?;
            continue;
        }
        if is_protected_text_path(&path) {
            out.insert(
                path.clone(),
                Entry {
                    kind: ProtectedKind::RestorableText,
                    bytes: read_no_follow_optional(&path)?,
                },
            );
        }
    }
    Ok(())
}

fn is_protected_text_path(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == ".last-branch" || name.ends_with(".json") || name.ends_with("-prompt.md")
}

fn read_no_follow_optional(path: &Path) -> TaskMgrResult<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        return Ok(Some(Vec::new()));
    }
    if !meta.is_file() {
        return Ok(None);
    }
    Ok(Some(fs::read(path)?))
}
