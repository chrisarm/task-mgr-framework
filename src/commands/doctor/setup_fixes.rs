//! Auto-fix functions for repairable setup issues detected by `doctor --setup`.
//!
//! Each function is idempotent: running it twice produces the same result.
//! **None of these functions ever modify `~/.claude/settings.json`** — that file
//! requires explicit user action. Settings.json issues have `auto_fixable: false`
//! and print a copy-pasteable `jq` command via the normal check output.
//!
//! # Function overview
//!
//! | Function                      | Action                                        |
//! |-------------------------------|-----------------------------------------------|
//! | `fix_install_skills`          | Copy `.md` files to `~/.claude/commands/`     |
//! | `fix_generate_project_config` | Write `.task-mgr/config.json`                 |
//! | `fix_patch_hook`              | Insert `LOOP_ALLOW_DESTRUCTIVE` bypass        |
//! | `fix_generate_claude_md`      | Generate template `CLAUDE.md`                 |
//! | `detect_additional_tools`     | Scan project for tool indicators              |

use std::path::{Path, PathBuf};

use crate::commands::doctor::setup_output::SetupFix;

/// Returns `true` if `path` is a symlink, **without following it**.
///
/// Uses `symlink_metadata` so a dangling symlink is still detected.
/// Returns `false` when the path does not exist or metadata is unavailable.
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Copy missing skill `.md` files from `source_dir` to `global_dir`.
///
/// For each name in `missing_skills`, copies `<source_dir>/<name>.md` to
/// `<global_dir>/<name>.md`. Creates `global_dir` if it does not exist.
///
/// # Idempotency
/// Overwrites the destination when it already exists — repeated runs produce
/// the same file content.
///
/// # Arguments
/// * `source_dir` — local skill directory (e.g., `<project>/.claude/commands/`)
/// * `global_dir` — installation target (e.g., `~/.claude/commands/`)
/// * `missing_skills` — skill names without the `.md` suffix
pub fn fix_install_skills(
    source_dir: &Path,
    global_dir: &Path,
    missing_skills: &[&str],
) -> Vec<SetupFix> {
    if missing_skills.is_empty() {
        return Vec::new();
    }

    // Ensure target directory exists before attempting any copies.
    if let Err(e) = std::fs::create_dir_all(global_dir) {
        return missing_skills
            .iter()
            .map(|name| SetupFix {
                name: format!("install_skill_{}", name.replace('-', "_")),
                action: format!("Failed to create {}: {e}", global_dir.display()),
                success: false,
            })
            .collect();
    }

    missing_skills
        .iter()
        .map(|name| {
            let fix_name = format!("install_skill_{}", name.replace('-', "_"));
            let src = source_dir.join(format!("{name}.md"));
            let dst = global_dir.join(format!("{name}.md"));

            if is_symlink(&dst) {
                return SetupFix {
                    name: fix_name,
                    action: format!("{} is a symlink — refusing to overwrite", dst.display()),
                    success: false,
                };
            }

            match std::fs::copy(&src, &dst) {
                Ok(_) => SetupFix {
                    name: fix_name,
                    action: format!("Copied {name}.md to {}", global_dir.display()),
                    success: true,
                },
                Err(e) => SetupFix {
                    name: fix_name,
                    action: format!("Failed to copy {} to {}: {e}", src.display(), dst.display()),
                    success: false,
                },
            }
        })
        .collect()
}

/// Generate `.task-mgr/config.json` with the given additional tool allowlist.
///
/// # Idempotency
/// If `db_dir/config.json` already exists, returns `success: true` without
/// touching the file. This protects existing project-specific configuration.
///
/// # Arguments
/// * `db_dir` — path to the `.task-mgr/` directory (must already exist)
/// * `detected_tools` — tool strings like `"Bash(docker:*)"` for
///   `additionalAllowedTools`. Use [`detect_additional_tools`] to generate this.
pub fn fix_generate_project_config(db_dir: &Path, detected_tools: &[String]) -> SetupFix {
    let config_path = db_dir.join("config.json");
    let fix_name = "generate_project_config".to_string();

    if is_symlink(&config_path) {
        return SetupFix {
            name: fix_name,
            action: format!("{} is a symlink — refusing to write", config_path.display()),
            success: false,
        };
    }

    if config_path.exists() {
        return SetupFix {
            name: fix_name,
            action: format!("{} already exists — skipped", config_path.display()),
            success: true,
        };
    }

    let tools_json: Vec<serde_json::Value> = detected_tools
        .iter()
        .map(|t| serde_json::Value::String(t.clone()))
        .collect();

    let config = serde_json::json!({
        "version": 1,
        "additionalAllowedTools": tools_json
    });

    let contents = match serde_json::to_string_pretty(&config) {
        Ok(s) => s,
        Err(e) => {
            return SetupFix {
                name: fix_name,
                action: format!("Failed to serialize config.json: {e}"),
                success: false,
            };
        }
    };

    match std::fs::write(&config_path, contents) {
        Ok(_) => SetupFix {
            name: fix_name,
            action: format!(
                "Generated {} with {} additional tool(s)",
                config_path.display(),
                detected_tools.len()
            ),
            success: true,
        },
        Err(e) => SetupFix {
            name: fix_name,
            action: format!("Failed to write {}: {e}", config_path.display()),
            success: false,
        },
    }
}

/// Patch a hook script to respect the `LOOP_ALLOW_DESTRUCTIVE` bypass variable.
///
/// The bypass line is inserted immediately after the `#!/bin/bash` shebang (or
/// prepended when no shebang is present). All existing script content is
/// preserved verbatim after the inserted line.
///
/// A `.bak` backup is created at `<hook_path>.bak` before any modification.
///
/// # Idempotency
/// If the script already contains `LOOP_ALLOW_DESTRUCTIVE`, returns
/// `success: true` without modifying the file or creating a backup.
///
/// # Bypass line inserted
/// ```bash
/// [ -n "$LOOP_ALLOW_DESTRUCTIVE" ] && exit 0
/// ```
pub fn fix_patch_hook(hook_path: &Path) -> SetupFix {
    let fix_name = "patch_hook".to_string();

    if is_symlink(hook_path) {
        return SetupFix {
            name: fix_name,
            action: format!("{} is a symlink — refusing to patch", hook_path.display()),
            success: false,
        };
    }

    let contents = match std::fs::read_to_string(hook_path) {
        Ok(c) => c,
        Err(e) => {
            return SetupFix {
                name: fix_name,
                action: format!("Failed to read {}: {e}", hook_path.display()),
                success: false,
            };
        }
    };

    // Idempotent: bypass already present — nothing to do.
    if contents.contains("LOOP_ALLOW_DESTRUCTIVE") {
        return SetupFix {
            name: fix_name,
            action: format!(
                "{} already has LOOP_ALLOW_DESTRUCTIVE bypass — skipped",
                hook_path.display()
            ),
            success: true,
        };
    }

    // Create a .bak backup before any modification.
    let bak_path = PathBuf::from(format!("{}.bak", hook_path.display()));
    if let Err(e) = std::fs::copy(hook_path, &bak_path) {
        return SetupFix {
            name: fix_name,
            action: format!("Failed to create backup {}: {e}", bak_path.display()),
            success: false,
        };
    }

    let bypass_line = "[ -n \"$LOOP_ALLOW_DESTRUCTIVE\" ] && exit 0\n";
    let patched = insert_hook_bypass(&contents, bypass_line);

    match std::fs::write(hook_path, patched) {
        Ok(_) => SetupFix {
            name: fix_name,
            action: format!(
                "Patched {} with LOOP_ALLOW_DESTRUCTIVE bypass (backup: {})",
                hook_path.display(),
                bak_path.display()
            ),
            success: true,
        },
        Err(e) => SetupFix {
            name: fix_name,
            action: format!("Failed to write patched hook {}: {e}", hook_path.display()),
            success: false,
        },
    }
}

/// Insert `bypass_line` after the shebang (`#!`) on line 1, or prepend it
/// when no shebang is present.
///
/// Visible for unit testing.
pub(crate) fn insert_hook_bypass(original: &str, bypass_line: &str) -> String {
    let lines: Vec<&str> = original.lines().collect();

    // Insert after shebang if the first line starts with "#!", otherwise prepend.
    let insert_pos = if lines.first().is_some_and(|l| l.starts_with("#!")) {
        1
    } else {
        0
    };

    let mut result = String::with_capacity(original.len() + bypass_line.len());
    let mut bypass_inserted = false;

    for (i, line) in lines.iter().enumerate() {
        if i == insert_pos && !bypass_inserted {
            result.push_str(bypass_line);
            bypass_inserted = true;
        }
        result.push_str(line);
        result.push('\n');
    }

    // Handle empty file or shebang-only file (insert_pos beyond end of lines).
    if !bypass_inserted {
        result.push_str(bypass_line);
    }

    result
}

/// Generate a template `CLAUDE.md` in `project_dir`.
///
/// The template records the database location so future loop agents have
/// project context without manual setup.
///
/// # Idempotency
/// If `project_dir/CLAUDE.md` already exists, returns `success: true` without
/// modifying the file. User-authored documentation is never overwritten.
///
/// # Arguments
/// * `project_dir` — project root directory
/// * `db_path`     — path to `.task-mgr/tasks.db`; shown in the template
pub fn fix_generate_claude_md(project_dir: &Path, db_path: &Path) -> SetupFix {
    let claude_md = project_dir.join("CLAUDE.md");
    let fix_name = "generate_claude_md".to_string();

    if is_symlink(&claude_md) {
        return SetupFix {
            name: fix_name,
            action: format!("{} is a symlink — refusing to write", claude_md.display()),
            success: false,
        };
    }

    if claude_md.exists() {
        return SetupFix {
            name: fix_name,
            action: format!("{} already exists — skipped", claude_md.display()),
            success: true,
        };
    }

    // Use a relative path in the template when possible.
    let db_display = db_path
        .strip_prefix(project_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| db_path.display().to_string());

    let template = format!(
        "# Project Notes\n\
        \n\
        ## Database Location\n\
        \n\
        The Ralph loop database is at `{db_display}` (relative to the project root).\n\
        \n\
        ## Task Files\n\
        \n\
        - PRD task lists: `.task-mgr/tasks/<prd-name>.json`\n\
        - Loop prompts: `.task-mgr/tasks/<prd-name>-prompt.md`\n\
        - Progress log: `.task-mgr/tasks/progress.txt`\n"
    );

    match std::fs::write(&claude_md, template) {
        Ok(_) => SetupFix {
            name: fix_name,
            action: format!("Generated template {}", claude_md.display()),
            success: true,
        },
        Err(e) => SetupFix {
            name: fix_name,
            action: format!("Failed to write {}: {e}", claude_md.display()),
            success: false,
        },
    }
}

/// Scan `project_dir` for common project-type indicators and return
/// `additionalAllowedTools` entries to add to `.task-mgr/config.json`.
///
/// | File / dir found    | Tool(s) added                                   |
/// |---------------------|-------------------------------------------------|
/// | `Dockerfile`        | `Bash(docker:*)`, `Bash(docker-compose:*)`      |
/// | `*.proto` files     | `Bash(protoc:*)`                                |
/// | `scripts/` dir      | `Bash(./scripts/*:*)`                           |
///
/// `python`, `uv`, `make` are already present in `CODING_ALLOWED_TOOLS` and
/// are not duplicated. Results are sorted and deduplicated.
pub fn detect_additional_tools(project_dir: &Path) -> Vec<String> {
    let mut tools = Vec::new();

    if project_dir.join("Dockerfile").exists() {
        tools.push("Bash(docker-compose:*)".to_string());
        tools.push("Bash(docker:*)".to_string());
    }

    // Scan top-level directory only (non-recursive) for speed.
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        let has_proto = entries
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("proto"));
        if has_proto {
            tools.push("Bash(protoc:*)".to_string());
        }
    }

    if project_dir.join("scripts").is_dir() {
        tools.push("Bash(./scripts/*:*)".to_string());
    }

    tools.sort();
    tools.dedup();
    tools
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ─── fix_install_skills ───────────────────────────────────────────────────

    #[test]
    fn test_fix_install_skills_copies_file() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        std::fs::write(src.path().join("tm-apply.md"), "# tm-apply").unwrap();

        let fixes = fix_install_skills(src.path(), dst.path(), &["tm-apply"]);

        assert_eq!(fixes.len(), 1);
        assert!(fixes[0].success, "expected success: {}", fixes[0].action);
        assert!(dst.path().join("tm-apply.md").exists());
    }

    #[test]
    fn test_fix_install_skills_empty_list_returns_empty() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();

        let fixes = fix_install_skills(src.path(), dst.path(), &[]);

        assert!(fixes.is_empty());
    }

    #[test]
    fn test_fix_install_skills_missing_source_returns_failure() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        // tm-apply.md does NOT exist in src

        let fixes = fix_install_skills(src.path(), dst.path(), &["tm-apply"]);

        assert_eq!(fixes.len(), 1);
        assert!(!fixes[0].success, "must fail when source file is missing");
    }

    /// Running twice should overwrite and report success both times.
    #[test]
    fn test_fix_install_skills_idempotent() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        std::fs::write(src.path().join("tm-apply.md"), "# tm-apply").unwrap();

        let fixes1 = fix_install_skills(src.path(), dst.path(), &["tm-apply"]);
        let fixes2 = fix_install_skills(src.path(), dst.path(), &["tm-apply"]);

        assert!(fixes1[0].success);
        assert!(fixes2[0].success);
        assert!(dst.path().join("tm-apply.md").exists());
    }

    /// `global_dir` is created automatically when it does not exist.
    #[test]
    fn test_fix_install_skills_creates_global_dir() {
        let src = TempDir::new().unwrap();
        let parent = TempDir::new().unwrap();
        let dst = parent.path().join("commands"); // does not exist yet
        std::fs::write(src.path().join("tm-learn.md"), "# tm-learn").unwrap();

        let fixes = fix_install_skills(src.path(), &dst, &["tm-learn"]);

        assert!(fixes[0].success, "must create dst dir: {}", fixes[0].action);
        assert!(dst.join("tm-learn.md").exists());
    }

    /// Multiple skills — each produces its own `SetupFix`.
    #[test]
    fn test_fix_install_skills_multiple_skills() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        std::fs::write(src.path().join("tm-apply.md"), "# apply").unwrap();
        std::fs::write(src.path().join("tm-learn.md"), "# learn").unwrap();

        let fixes = fix_install_skills(src.path(), dst.path(), &["tm-apply", "tm-learn"]);

        assert_eq!(fixes.len(), 2);
        assert!(fixes.iter().all(|f| f.success));
    }

    // ─── fix_generate_project_config ─────────────────────────────────────────

    #[test]
    fn test_fix_generate_project_config_creates_file() {
        let dir = TempDir::new().unwrap();
        let tools = vec!["Bash(docker:*)".to_string()];

        let fix = fix_generate_project_config(dir.path(), &tools);

        assert!(fix.success, "expected success: {}", fix.action);
        let path = dir.path().join("config.json");
        assert!(path.exists());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["additionalAllowedTools"][0], "Bash(docker:*)");
    }

    #[test]
    fn test_fix_generate_project_config_empty_tools() {
        let dir = TempDir::new().unwrap();

        let fix = fix_generate_project_config(dir.path(), &[]);

        assert!(fix.success);
        let contents = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(v["additionalAllowedTools"], serde_json::json!([]));
    }

    /// Existing config.json must not be overwritten.
    #[test]
    fn test_fix_generate_project_config_idempotent() {
        let dir = TempDir::new().unwrap();
        let original = r#"{"version":1,"additionalAllowedTools":["Bash(custom:*)"]}"#;
        std::fs::write(dir.path().join("config.json"), original).unwrap();

        let fix = fix_generate_project_config(dir.path(), &["Bash(docker:*)".to_string()]);

        assert!(fix.success, "idempotent run must succeed: {}", fix.action);
        let contents = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert_eq!(
            contents, original,
            "existing config.json must not be modified"
        );
    }

    // ─── fix_patch_hook ───────────────────────────────────────────────────────

    #[test]
    fn test_fix_patch_hook_inserts_bypass_after_shebang() {
        let dir = TempDir::new().unwrap();
        let hook = dir.path().join("guard-destructive.sh");
        std::fs::write(&hook, "#!/bin/bash\necho 'guard'\n").unwrap();

        let fix = fix_patch_hook(&hook);

        assert!(fix.success, "expected success: {}", fix.action);
        let patched = std::fs::read_to_string(&hook).unwrap();
        assert!(
            patched.contains("LOOP_ALLOW_DESTRUCTIVE"),
            "bypass must be in patched file: {patched}"
        );
        assert!(
            patched.contains("echo 'guard'"),
            "original content must be preserved: {patched}"
        );
        // Bypass must appear before the original guard command.
        let bypass_pos = patched.find("LOOP_ALLOW_DESTRUCTIVE").unwrap();
        let guard_pos = patched.find("echo 'guard'").unwrap();
        assert!(
            bypass_pos < guard_pos,
            "bypass must precede original commands"
        );
    }

    #[test]
    fn test_fix_patch_hook_creates_bak_backup() {
        let dir = TempDir::new().unwrap();
        let hook = dir.path().join("guard-destructive.sh");
        std::fs::write(&hook, "#!/bin/bash\necho 'guard'\n").unwrap();

        fix_patch_hook(&hook);

        let bak = PathBuf::from(format!("{}.bak", hook.display()));
        assert!(bak.exists(), "backup file must be created");
        let bak_contents = std::fs::read_to_string(&bak).unwrap();
        assert!(
            bak_contents.contains("echo 'guard'"),
            "backup must contain original content"
        );
        assert!(
            !bak_contents.contains("LOOP_ALLOW_DESTRUCTIVE"),
            "backup must NOT contain the bypass"
        );
    }

    /// When the bypass is already present, the file must not be touched.
    #[test]
    fn test_fix_patch_hook_idempotent_bypass_already_present() {
        let dir = TempDir::new().unwrap();
        let hook = dir.path().join("guard-destructive.sh");
        let original = "#!/bin/bash\n[ -n \"$LOOP_ALLOW_DESTRUCTIVE\" ] && exit 0\necho 'guard'\n";
        std::fs::write(&hook, original).unwrap();

        let fix = fix_patch_hook(&hook);

        assert!(fix.success);
        let contents = std::fs::read_to_string(&hook).unwrap();
        assert_eq!(
            contents, original,
            "file must not be modified when bypass already present"
        );
    }

    #[test]
    fn test_fix_patch_hook_missing_file_returns_failure() {
        let dir = TempDir::new().unwrap();
        let hook = dir.path().join("nonexistent.sh");

        let fix = fix_patch_hook(&hook);

        assert!(!fix.success, "must fail for nonexistent hook file");
    }

    // ─── insert_hook_bypass ───────────────────────────────────────────────────

    #[test]
    fn test_insert_hook_bypass_after_shebang() {
        let result = insert_hook_bypass("#!/bin/bash\necho 'guard'\n", "BYPASS\n");
        assert_eq!(result, "#!/bin/bash\nBYPASS\necho 'guard'\n");
    }

    #[test]
    fn test_insert_hook_bypass_prepends_when_no_shebang() {
        let result = insert_hook_bypass("echo 'guard'\n", "BYPASS\n");
        assert_eq!(result, "BYPASS\necho 'guard'\n");
    }

    #[test]
    fn test_insert_hook_bypass_shebang_only() {
        let result = insert_hook_bypass("#!/bin/bash\n", "BYPASS\n");
        assert_eq!(result, "#!/bin/bash\nBYPASS\n");
    }

    #[test]
    fn test_insert_hook_bypass_empty_file() {
        let result = insert_hook_bypass("", "BYPASS\n");
        assert_eq!(result, "BYPASS\n");
    }

    // ─── fix_generate_claude_md ───────────────────────────────────────────────

    #[test]
    fn test_fix_generate_claude_md_creates_file() {
        let project = TempDir::new().unwrap();
        let db_path = project.path().join(".task-mgr").join("tasks.db");

        let fix = fix_generate_claude_md(project.path(), &db_path);

        assert!(fix.success, "expected success: {}", fix.action);
        let claude_md = project.path().join("CLAUDE.md");
        assert!(claude_md.exists());
        let contents = std::fs::read_to_string(claude_md).unwrap();
        assert!(contents.contains("# Project Notes"), "must contain header");
        assert!(
            contents.contains(".task-mgr"),
            "must mention db path: {contents}"
        );
    }

    /// Existing CLAUDE.md must not be overwritten.
    #[test]
    fn test_fix_generate_claude_md_idempotent() {
        let project = TempDir::new().unwrap();
        let db_path = project.path().join(".task-mgr/tasks.db");
        let claude_md = project.path().join("CLAUDE.md");
        std::fs::write(&claude_md, "# My custom notes").unwrap();

        let fix = fix_generate_claude_md(project.path(), &db_path);

        assert!(fix.success);
        let contents = std::fs::read_to_string(&claude_md).unwrap();
        assert_eq!(
            contents, "# My custom notes",
            "existing CLAUDE.md must not be overwritten"
        );
    }

    // ─── detect_additional_tools ─────────────────────────────────────────────

    #[test]
    fn test_detect_additional_tools_empty_project() {
        let dir = TempDir::new().unwrap();

        let tools = detect_additional_tools(dir.path());

        assert!(tools.is_empty(), "empty project must produce no tools");
    }

    #[test]
    fn test_detect_additional_tools_dockerfile() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM ubuntu").unwrap();

        let tools = detect_additional_tools(dir.path());

        assert!(
            tools.contains(&"Bash(docker:*)".to_string()),
            "must detect docker"
        );
        assert!(
            tools.contains(&"Bash(docker-compose:*)".to_string()),
            "must detect docker-compose"
        );
    }

    #[test]
    fn test_detect_additional_tools_proto() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("service.proto"), "syntax = \"proto3\";").unwrap();

        let tools = detect_additional_tools(dir.path());

        assert!(
            tools.contains(&"Bash(protoc:*)".to_string()),
            "must detect protoc"
        );
    }

    #[test]
    fn test_detect_additional_tools_scripts_dir() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("scripts")).unwrap();

        let tools = detect_additional_tools(dir.path());

        assert!(
            tools.contains(&"Bash(./scripts/*:*)".to_string()),
            "must detect scripts dir"
        );
    }

    // ─── symlink safety ───────────────────────────────────────────────────────

    /// fix_patch_hook must refuse to modify a symlink target.
    #[test]
    #[cfg(unix)]
    fn test_fix_patch_hook_symlink_returns_failure() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real-hook.sh");
        std::fs::write(&target, "#!/bin/bash\necho 'guard'\n").unwrap();
        let link = dir.path().join("guard-destructive.sh");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let fix = fix_patch_hook(&link);

        assert!(
            !fix.success,
            "symlink hook must be rejected: {}",
            fix.action
        );
        assert!(
            fix.action.contains("symlink"),
            "message must mention symlink: {}",
            fix.action
        );
    }

    /// fix_install_skills must refuse to overwrite a symlink at the destination.
    #[test]
    #[cfg(unix)]
    fn test_fix_install_skills_symlink_dst_returns_failure() {
        let src = TempDir::new().unwrap();
        let dst_dir = TempDir::new().unwrap();
        std::fs::write(src.path().join("tm-apply.md"), "# tm-apply").unwrap();

        // Plant a symlink at the expected destination path.
        let real_target = src.path().join("real-tm-apply.md");
        std::fs::write(&real_target, "# real").unwrap();
        let link = dst_dir.path().join("tm-apply.md");
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let fixes = fix_install_skills(src.path(), dst_dir.path(), &["tm-apply"]);

        assert_eq!(fixes.len(), 1);
        assert!(
            !fixes[0].success,
            "symlink dst must be rejected: {}",
            fixes[0].action
        );
        assert!(
            fixes[0].action.contains("symlink"),
            "message must mention symlink: {}",
            fixes[0].action
        );
    }

    /// Results must be sorted and contain no duplicates.
    #[test]
    fn test_detect_additional_tools_sorted_and_deduplicated() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM ubuntu").unwrap();
        std::fs::write(dir.path().join("a.proto"), "syntax = \"proto3\";").unwrap();
        std::fs::create_dir(dir.path().join("scripts")).unwrap();

        let tools = detect_additional_tools(dir.path());

        let mut sorted = tools.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(tools, sorted, "tools must be sorted and deduplicated");
    }
}
