//! Embedded skill registry and global staging into `~/.claude/commands/`.
//!
//! The `.claude/commands/*.md` skill files in this repo are compiled into the
//! binary via `include_str!` so they version with the installed binary
//! (`cargo install` upgrade = skills upgrade). [`stage_skills`] idempotently
//! writes them into a target commands directory, guarded by a hash manifest
//! (`.task-mgr-skills.json`, md5 of the last-installed bytes per skill) so a
//! file the operator deliberately edited is never silently clobbered:
//!
//! - symlink (incl. dangling) → never touched, even with `force`
//! - missing → installed
//! - byte-identical to embedded → adopted (hash recorded), nothing written
//! - matches the manifest hash (unmodified since we installed it) → refreshed
//! - anything else → user-modified: skipped unless `force`
//!
//! Files whose names are not in the registry (e.g. an operator's own skills)
//! are never touched. All writes (skills and manifest) are atomic via a
//! same-directory temp file + rename, so concurrent runs degrade to
//! last-writer-wins with no torn state.
//!
//! # Known limitation: last binary to stage wins
//!
//! The manifest records "what task-mgr last installed", not a version. An
//! *older* binary staging after a newer one sees `current hash == manifest
//! hash` and will refresh *downward*. Acceptable for a single installed
//! binary; beware running `./target/debug/task-mgr init` from an old branch
//! against the real `$HOME`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A skill markdown file compiled into the binary.
pub struct EmbeddedSkill {
    /// File stem under the commands directory (`<name>.md`).
    pub name: &'static str,
    /// Full file content as embedded at compile time.
    pub content: &'static str,
}

/// Deprecation banner prepended to the `prd-tasks` alias content. The alias
/// ships the current `/tasks` skill under the legacy name; the banner is the
/// only channel through which the deprecation can reach its audience (it lands
/// in the agent's context when `/prd-tasks` is invoked).
pub const PRD_TASKS_DEPRECATION_BANNER: &str =
    "> **Deprecated**: `/prd-tasks` is an alias for `/tasks`; update references to `/tasks`.\n\n";

/// All skill files managed by task-mgr. Adding a `.md` file to
/// `.claude/commands/` requires adding an entry here — enforced by
/// `registry_matches_commands_directory`.
pub const EMBEDDED_SKILLS: &[EmbeddedSkill] = &[
    EmbeddedSkill {
        name: "compound",
        content: include_str!("../.claude/commands/compound.md"),
    },
    EmbeddedSkill {
        name: "plan-tasks",
        content: include_str!("../.claude/commands/plan-tasks.md"),
    },
    EmbeddedSkill {
        name: "prd",
        content: include_str!("../.claude/commands/prd.md"),
    },
    EmbeddedSkill {
        name: "review-loop",
        content: include_str!("../.claude/commands/review-loop.md"),
    },
    EmbeddedSkill {
        name: "review-plan",
        content: include_str!("../.claude/commands/review-plan.md"),
    },
    EmbeddedSkill {
        name: "spike",
        content: include_str!("../.claude/commands/spike.md"),
    },
    EmbeddedSkill {
        name: "tasks",
        content: include_str!("../.claude/commands/tasks.md"),
    },
    EmbeddedSkill {
        name: "tm-apply",
        content: include_str!("../.claude/commands/tm-apply.md"),
    },
    EmbeddedSkill {
        name: "tm-decisions",
        content: include_str!("../.claude/commands/tm-decisions.md"),
    },
    EmbeddedSkill {
        name: "tm-invalidate",
        content: include_str!("../.claude/commands/tm-invalidate.md"),
    },
    EmbeddedSkill {
        name: "tm-learn",
        content: include_str!("../.claude/commands/tm-learn.md"),
    },
    EmbeddedSkill {
        name: "tm-next",
        content: include_str!("../.claude/commands/tm-next.md"),
    },
    EmbeddedSkill {
        name: "tm-recall",
        content: include_str!("../.claude/commands/tm-recall.md"),
    },
    EmbeddedSkill {
        name: "tm-status",
        content: include_str!("../.claude/commands/tm-status.md"),
    },
    // Deprecated alias for /tasks under its pre-rename name. Keep the banner
    // literal in sync with PRD_TASKS_DEPRECATION_BANNER (pinned by test).
    EmbeddedSkill {
        name: "prd-tasks",
        content: concat!(
            "> **Deprecated**: `/prd-tasks` is an alias for `/tasks`; update references to `/tasks`.\n\n",
            include_str!("../.claude/commands/tasks.md")
        ),
    },
];

/// Alias entries that exist only as staged files, with no source `.md` of
/// their own in `.claude/commands/`.
const ALIAS_NAMES: &[&str] = &["prd-tasks"];

/// Skill names doctor expects in the global commands directory (aliases
/// excluded — doctor should not demand the deprecated name).
pub fn expected_skill_names() -> Vec<&'static str> {
    EMBEDDED_SKILLS
        .iter()
        .map(|s| s.name)
        .filter(|n| !ALIAS_NAMES.contains(n))
        .collect()
}

/// Embedded content for a registry skill name, if it exists.
pub fn embedded_content(name: &str) -> Option<&'static str> {
    EMBEDDED_SKILLS
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.content)
}

/// The manifest's recorded last-installed hash per skill name. Empty when the
/// manifest is missing or unreadable. Read-only — used by doctor's freshness
/// check to distinguish "stale but task-mgr-owned" from "locally modified"
/// without writing anything.
pub fn manifest_hashes(commands_dir: &Path) -> BTreeMap<String, String> {
    fs::read(commands_dir.join(MANIFEST_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
        .map(|m| m.skills)
        .unwrap_or_default()
}

/// Hex md5 of `bytes` — exposed so doctor's read-only freshness check uses the
/// exact same hash as the staging writer.
pub fn content_hash(bytes: &[u8]) -> String {
    md5_hex(bytes)
}

/// Manifest file name inside the commands directory. Hidden and non-`.md` so
/// Claude Code's command discovery never sees it; co-located so that deleting
/// or recreating the directory correctly reverts to fresh-install semantics.
pub const MANIFEST_FILE: &str = ".task-mgr-skills.json";

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    version: u32,
    /// skill name → md5 hex of the bytes task-mgr last installed.
    skills: BTreeMap<String, String>,
}

/// Result of one [`stage_skills`] pass. All name lists hold registry names.
#[derive(Default)]
pub struct StageOutcome {
    pub installed: Vec<&'static str>,
    pub refreshed: Vec<&'static str>,
    pub up_to_date: Vec<&'static str>,
    /// Overwritten despite local modifications (only possible with `force`).
    pub overwrote_modified: Vec<&'static str>,
    pub skipped_modified: Vec<&'static str>,
    pub skipped_symlink: Vec<&'static str>,
    /// No manifest file existed before this pass — true on the very first run
    /// in a directory (fresh install OR first run after upgrading from the
    /// old copy-based flow). Callers use this to explain why pre-existing
    /// files show up as "local edits".
    pub manifest_missing: bool,
    /// Non-fatal problems (directory not writable, corrupt manifest, IO
    /// errors). Staging is best-effort; callers warn and move on.
    pub errors: Vec<String>,
}

impl StageOutcome {
    /// True when nothing was written and nothing needs operator attention.
    pub fn is_noop(&self) -> bool {
        self.installed.is_empty()
            && self.refreshed.is_empty()
            && self.overwrote_modified.is_empty()
            && self.skipped_modified.is_empty()
            && self.skipped_symlink.is_empty()
            && self.errors.is_empty()
    }
}

fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", md5::compute(bytes))
}

/// Returns `true` if `path` is a symlink, **without following it** (a
/// dangling symlink is still detected). Shared with doctor's setup fixes so
/// the symlink policy lives in one place.
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Atomic write: same-directory temp file + rename. The temp name carries the
/// pid so concurrent stagers cannot collide on it; rename makes the final
/// content appear all-at-once (last writer wins).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".to_string());
    let tmp = path.with_file_name(format!(".{}.{}.tmp", file_name, std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Idempotently write the embedded skills into `commands_dir`.
///
/// Never follows or replaces symlinks, never touches files whose names are
/// not in the registry, and never returns an error — problems are collected
/// in [`StageOutcome::errors`] for the caller to surface (staging is
/// best-effort by contract).
pub fn stage_skills(commands_dir: &Path, force: bool) -> StageOutcome {
    let mut outcome = StageOutcome::default();

    if let Err(e) = fs::create_dir_all(commands_dir) {
        outcome.errors.push(format!(
            "cannot write to {} ({e}); skipped staging",
            commands_dir.display()
        ));
        return outcome;
    }

    let manifest_path = commands_dir.join(MANIFEST_FILE);
    let mut manifest = match fs::read(&manifest_path) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(m) => m,
            Err(_) => {
                outcome.errors.push(format!(
                    "manifest {} is unreadable; rebuilding — files matching the bundled \
                     content were re-adopted; differing files were kept in case they are \
                     local edits (use --force-skills to replace them)",
                    manifest_path.display()
                ));
                Manifest::default()
            }
        },
        Err(_) => {
            outcome.manifest_missing = true;
            Manifest::default()
        }
    };
    manifest.version = 1;

    let mut manifest_dirty = false;
    for skill in EMBEDDED_SKILLS {
        let path = commands_dir.join(format!("{}.md", skill.name));

        if is_symlink(&path) {
            outcome.skipped_symlink.push(skill.name);
            continue;
        }

        let embedded = skill.content.as_bytes();
        let record = |manifest: &mut Manifest, dirty: &mut bool| {
            let hex = md5_hex(embedded);
            if manifest.skills.get(skill.name) != Some(&hex) {
                manifest.skills.insert(skill.name.to_string(), hex);
                *dirty = true;
            }
        };

        match fs::read(&path) {
            Err(_) => match write_atomic(&path, embedded) {
                Ok(()) => {
                    outcome.installed.push(skill.name);
                    record(&mut manifest, &mut manifest_dirty);
                }
                Err(e) => outcome
                    .errors
                    .push(format!("failed to write {}: {e}", path.display())),
            },
            Ok(existing) => {
                if existing == embedded {
                    // Already current — adopt silently so future refreshes work.
                    outcome.up_to_date.push(skill.name);
                    record(&mut manifest, &mut manifest_dirty);
                    continue;
                }
                let installed_by_us = manifest.skills.get(skill.name) == Some(&md5_hex(&existing));
                if installed_by_us || force {
                    match write_atomic(&path, embedded) {
                        Ok(()) => {
                            if installed_by_us {
                                outcome.refreshed.push(skill.name);
                            } else {
                                outcome.overwrote_modified.push(skill.name);
                            }
                            record(&mut manifest, &mut manifest_dirty);
                        }
                        Err(e) => outcome
                            .errors
                            .push(format!("failed to write {}: {e}", path.display())),
                    }
                } else {
                    outcome.skipped_modified.push(skill.name);
                }
            }
        }
    }

    if manifest_dirty {
        match serde_json::to_vec_pretty(&manifest) {
            Ok(bytes) => {
                if let Err(e) = write_atomic(&manifest_path, &bytes) {
                    outcome
                        .errors
                        .push(format!("failed to write {}: {e}", manifest_path.display()));
                }
            }
            Err(e) => outcome
                .errors
                .push(format!("failed to serialize manifest: {e}")),
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    fn skill(name: &str) -> &'static EmbeddedSkill {
        EMBEDDED_SKILLS.iter().find(|s| s.name == name).unwrap()
    }

    #[test]
    fn registry_matches_commands_directory() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".claude/commands");
        let on_disk: BTreeSet<String> = fs::read_dir(&dir)
            .expect("read .claude/commands")
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension().is_some_and(|x| x == "md"))
                    .then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
            })
            .collect();
        let in_registry: BTreeSet<String> = EMBEDDED_SKILLS
            .iter()
            .map(|s| s.name.to_string())
            .filter(|n| !ALIAS_NAMES.contains(&n.as_str()))
            .collect();
        assert_eq!(
            on_disk, in_registry,
            "every .claude/commands/*.md must have an EMBEDDED_SKILLS entry (and vice versa)"
        );
    }

    #[test]
    fn prd_tasks_alias_is_banner_plus_tasks_content() {
        let alias = skill("prd-tasks").content;
        let tasks = skill("tasks").content;
        assert!(alias.starts_with(PRD_TASKS_DEPRECATION_BANNER));
        assert_eq!(&alias[PRD_TASKS_DEPRECATION_BANNER.len()..], tasks);
    }

    #[test]
    fn expected_names_exclude_alias() {
        let names = expected_skill_names();
        assert!(!names.contains(&"prd-tasks"));
        assert!(names.contains(&"tasks"));
        assert_eq!(names.len(), EMBEDDED_SKILLS.len() - ALIAS_NAMES.len());
    }

    #[test]
    fn fresh_install_writes_everything_and_manifest() {
        let dir = TempDir::new().unwrap();
        let outcome = stage_skills(dir.path(), false);
        assert_eq!(outcome.installed.len(), EMBEDDED_SKILLS.len());
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        for s in EMBEDDED_SKILLS {
            let on_disk = fs::read_to_string(dir.path().join(format!("{}.md", s.name))).unwrap();
            assert_eq!(on_disk, s.content);
        }
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        assert_eq!(manifest.skills.len(), EMBEDDED_SKILLS.len());
    }

    #[test]
    fn second_run_is_noop() {
        let dir = TempDir::new().unwrap();
        stage_skills(dir.path(), false);
        let outcome = stage_skills(dir.path(), false);
        assert!(outcome.is_noop(), "second run must change nothing");
        assert_eq!(outcome.up_to_date.len(), EMBEDDED_SKILLS.len());
    }

    #[test]
    fn refreshes_unmodified_stale_copy() {
        let dir = TempDir::new().unwrap();
        stage_skills(dir.path(), false);
        // Simulate an older binary's install: rewrite a skill AND its manifest
        // hash so the file is "unmodified since we installed it".
        let path = dir.path().join("spike.md");
        fs::write(&path, "old embedded content").unwrap();
        let manifest_path = dir.path().join(MANIFEST_FILE);
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest
            .skills
            .insert("spike".into(), md5_hex(b"old embedded content"));
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let outcome = stage_skills(dir.path(), false);
        assert_eq!(outcome.refreshed, vec!["spike"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), skill("spike").content);
    }

    #[test]
    fn skips_user_modified_without_force() {
        let dir = TempDir::new().unwrap();
        stage_skills(dir.path(), false);
        let path = dir.path().join("spike.md");
        fs::write(&path, "my local customization").unwrap();

        let outcome = stage_skills(dir.path(), false);
        assert_eq!(outcome.skipped_modified, vec!["spike"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), "my local customization");
    }

    #[test]
    fn force_overwrites_user_modified() {
        let dir = TempDir::new().unwrap();
        stage_skills(dir.path(), false);
        let path = dir.path().join("spike.md");
        fs::write(&path, "my local customization").unwrap();

        let outcome = stage_skills(dir.path(), true);
        assert_eq!(outcome.overwrote_modified, vec!["spike"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), skill("spike").content);
        // And the manifest now tracks it again:
        let outcome = stage_skills(dir.path(), false);
        assert!(outcome.is_noop());
    }

    #[test]
    fn adopts_already_current_file_without_manifest() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join("spike.md"), skill("spike").content).unwrap();

        let outcome = stage_skills(dir.path(), false);
        assert!(outcome.up_to_date.contains(&"spike"));
        assert!(!outcome.skipped_modified.contains(&"spike"));
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        assert!(manifest.skills.contains_key("spike"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_even_with_force() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real-file.md");
        fs::write(&target, "elsewhere").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("spike.md")).unwrap();
        // Dangling symlink for a second skill:
        std::os::unix::fs::symlink(dir.path().join("nope"), dir.path().join("prd.md")).unwrap();

        let outcome = stage_skills(dir.path(), true);
        assert!(outcome.skipped_symlink.contains(&"spike"));
        assert!(outcome.skipped_symlink.contains(&"prd"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "elsewhere");
    }

    #[test]
    fn corrupt_manifest_is_rebuilt_without_clobbering_modified_files() {
        let dir = TempDir::new().unwrap();
        stage_skills(dir.path(), false);
        let path = dir.path().join("spike.md");
        fs::write(&path, "my local customization").unwrap();
        fs::write(dir.path().join(MANIFEST_FILE), b"{ not json").unwrap();

        let outcome = stage_skills(dir.path(), false);
        assert!(outcome.errors.iter().any(|e| e.contains("unreadable")));
        assert_eq!(outcome.skipped_modified, vec!["spike"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), "my local customization");
        // Manifest was rebuilt for the adopted (current) files:
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        assert!(manifest.skills.contains_key("tasks"));
        assert!(!manifest.skills.contains_key("spike"));
    }

    #[test]
    fn never_touches_foreign_files() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join("prep-env.md"), "operator's own skill").unwrap();

        stage_skills(dir.path(), true);
        assert_eq!(
            fs::read_to_string(dir.path().join("prep-env.md")).unwrap(),
            "operator's own skill"
        );
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        assert!(!manifest.skills.contains_key("prep-env"));
    }

    #[test]
    fn unwritable_directory_yields_single_error() {
        // A path that create_dir_all cannot make: under an existing FILE.
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "x").unwrap();
        let outcome = stage_skills(&blocker.join("commands"), false);
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.installed.is_empty());
    }
}
