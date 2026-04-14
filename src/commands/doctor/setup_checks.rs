//! Setup check functions for `doctor --setup`.
//!
//! Each function is a pure file-reader: it takes a path (or a set of paths),
//! reads what it needs, and returns a `SetupCheck` or `Vec<SetupCheck>`.
//! No check mutates the filesystem.
//!
//! # Function signatures
//!
//! | Function                  | Path argument(s)                       | Returns           |
//! |---------------------------|----------------------------------------|-------------------|
//! | `check_default_mode`      | `settings_path: &Path`                 | `SetupCheck`      |
//! | `check_deny_conflicts`    | `settings_path: &Path`                 | `Vec<SetupCheck>` |
//! | `check_hook_bypass`       | `hook_path: &Path`                     | `SetupCheck`      |
//! | `check_skills_installed`  | `global_dir: &Path, expected: &[&str]` | `Vec<SetupCheck>` |
//! | `check_project_config`    | `db_dir: &Path`                        | `SetupCheck`      |
//! | `check_claude_md`         | `project_dir: &Path`                   | `SetupCheck`      |

use std::path::Path;

use crate::commands::doctor::setup_output::{SetupCategory, SetupCheck, SetupSeverity};
use crate::loop_engine::config::CODING_ALLOWED_TOOLS;

/// Read and parse `settings_path`.
///
/// # Returns
/// - `Ok(None)`   — file is absent (callers treat this as "no config, assume OK")
/// - `Ok(Some(v))` — file exists and was parsed successfully
/// - `Err(msg)` — file exists but could not be read or is not valid JSON;
///   `msg` is a human-readable error string for embedding in a [`SetupCheck`] message
fn read_settings_json(settings_path: &Path) -> Result<Option<serde_json::Value>, String> {
    if !settings_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(settings_path)
        .map_err(|e| format!("Could not read settings.json: {e}"))?;

    let json = serde_json::from_str(&contents)
        .map_err(|e| format!("settings.json is not valid JSON: {e}"))?;

    Ok(Some(json))
}

/// Expected task-mgr skill names (`.md` files in `~/.claude/commands/`).
pub const EXPECTED_SKILLS: &[&str] = &[
    "tm-apply",
    "tm-learn",
    "tm-recall",
    "tm-invalidate",
    "tm-status",
    "tm-next",
];

/// Check that `permissions.defaultMode` is not set to `"default"`.
///
/// `defaultMode: "default"` combined with `--allowedTools` causes Claude to
/// prompt for confirmation for every tool call, blocking unattended loop runs.
///
/// # Returns
/// - `Blocker` if `defaultMode` is `"default"`
/// - `Pass` if `defaultMode` is absent, `"auto"`, or any other non-blocking value
/// - `Warning` if `settings.json` exists but cannot be read or parsed
pub fn check_default_mode(settings_path: &Path) -> SetupCheck {
    let name = "default_mode".to_string();
    let category = SetupCategory::Permissions;

    let json = match read_settings_json(settings_path) {
        Ok(None) => {
            return SetupCheck {
                category,
                name,
                message: "settings.json not found — defaultMode not configured".to_string(),
                severity: SetupSeverity::Pass,
                fix_command: None,
                auto_fixable: false,
            };
        }
        Ok(Some(v)) => v,
        Err(msg) => {
            return SetupCheck {
                category,
                name,
                message: msg,
                severity: SetupSeverity::Warning,
                fix_command: None,
                auto_fixable: false,
            };
        }
    };

    let mode = json
        .get("permissions")
        .and_then(|p| p.get("defaultMode"))
        .and_then(|m| m.as_str());

    match mode {
        Some("default") => SetupCheck {
            category,
            name,
            message: concat!(
                "permissions.defaultMode is \"default\" — this blocks loop tool calls.",
                " Set to \"auto\" or \"acceptEdits\"."
            )
            .to_string(),
            severity: SetupSeverity::Blocker,
            fix_command: Some(format!(
                "jq '.permissions.defaultMode = \"auto\"' {path} | sponge {path}",
                path = settings_path.display()
            )),
            auto_fixable: false,
        },
        _ => SetupCheck {
            category,
            name,
            message: format!(
                "permissions.defaultMode is {} — OK",
                mode.map_or("not set".to_string(), |m| format!("\"{m}\""))
            ),
            severity: SetupSeverity::Pass,
            fix_command: None,
            auto_fixable: false,
        },
    }
}

/// Check that no `permissions.deny` rule conflicts with `CODING_ALLOWED_TOOLS`.
///
/// A deny rule that matches an allowed tool will block the loop from running
/// that tool even though it is in the allow list.
///
/// Three forms of conflict are detected:
/// 1. **Exact match** — deny rule is identical to an allowed tool entry.
/// 2. **Bare prefix** — deny rule has no `(` and equals the tool-name prefix of
///    any parameterized allowed tool (e.g. `Bash` conflicts with `Bash(cargo:*)`).
/// 3. **Wildcard suffix** — deny rule ends with `(*)` and the tool-name prefix
///    matches (e.g. `Bash(*)` conflicts with `Bash(cargo:*)`).
///
/// # Returns
/// One `Blocker` `SetupCheck` per conflicting deny rule. Returns an empty `Vec`
/// when there are no conflicts, or when `settings.json` is absent/unreadable.
pub fn check_deny_conflicts(settings_path: &Path) -> Vec<SetupCheck> {
    let json = match read_settings_json(settings_path) {
        Ok(Some(v)) => v,
        _ => return Vec::new(),
    };

    let deny_rules: Vec<String> = json
        .get("permissions")
        .and_then(|p| p.get("deny"))
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let allowed_tools: Vec<&str> = CODING_ALLOWED_TOOLS.split(',').collect();

    deny_rules
        .iter()
        .filter(|rule| {
            allowed_tools
                .iter()
                .any(|allowed| deny_rule_conflicts_with(rule, allowed))
        })
        .map(|rule| {
            let safe_name = rule.replace(['(', ')', ':', '*'], "_");
            SetupCheck {
                category: SetupCategory::Permissions,
                name: format!("deny_conflict_{safe_name}"),
                message: format!(
                    "Deny rule {rule:?} conflicts with CODING_ALLOWED_TOOLS — \
                     this will block loop tool calls"
                ),
                severity: SetupSeverity::Blocker,
                fix_command: Some(format!(
                    "jq 'del(.permissions.deny[] | select(. == \"{rule}\"))' {path} | sponge {path}",
                    path = settings_path.display()
                )),
                auto_fixable: false,
            }
        })
        .collect()
}

/// Return `true` when `deny_rule` would block `allowed_tool`.
///
/// Handles three cases:
/// - Exact: `deny_rule == allowed_tool`
/// - Bare prefix: `deny_rule` has no `(` and matches the tool-name prefix of
///   a parameterized `allowed_tool`, e.g. `Bash` vs `Bash(cargo:*)`.
/// - Wildcard: `deny_rule` ends with `(*)` and the prefix matches, e.g.
///   `Bash(*)` vs `Bash(cargo:*)`.
///
/// The tool-name prefix of a string is the part before the first `(`.
/// Prefix comparison is exact so `Bash` does not match `BashExtended(foo:*)`.
fn deny_rule_conflicts_with(deny_rule: &str, allowed_tool: &str) -> bool {
    // 1. Exact match — covers precise entries like "Bash(cargo:*)" or "Read".
    if deny_rule == allowed_tool {
        return true;
    }

    let deny_prefix = deny_rule.split('(').next().unwrap_or(deny_rule);
    let allowed_prefix = allowed_tool.split('(').next().unwrap_or(allowed_tool);

    // Different tool families — no conflict regardless of suffix.
    if deny_prefix != allowed_prefix {
        return false;
    }

    // 2. Bare deny rule (e.g. "Bash") blocks all parameterised variants
    //    such as "Bash(cargo:*)".
    if !deny_rule.contains('(') && allowed_tool.contains('(') {
        return true;
    }

    // 3. Wildcard deny rule (e.g. "Bash(*)") blocks all parameterised variants.
    if deny_rule.ends_with("(*)") {
        return true;
    }

    false
}

/// Check that a hook script respects the `LOOP_ALLOW_DESTRUCTIVE` bypass.
///
/// The loop sets `LOOP_ALLOW_DESTRUCTIVE=1` in the subprocess environment so
/// that destructive-guard hooks can allow all operations during automated runs.
/// A hook that does not check this variable will block loop tool calls.
///
/// # Returns
/// - `Pass` if the hook file does not exist (nothing to bypass)
/// - `Pass` if the hook contains `LOOP_ALLOW_DESTRUCTIVE`
/// - `Warning` if the hook exists but does not contain the bypass
/// - `Warning` if the hook file cannot be read
pub fn check_hook_bypass(hook_path: &Path) -> SetupCheck {
    let name = "hook_bypass".to_string();
    let category = SetupCategory::Hooks;

    if !hook_path.exists() {
        return SetupCheck {
            category,
            name,
            message: "Hook file not found — no bypass check needed".to_string(),
            severity: SetupSeverity::Pass,
            fix_command: None,
            auto_fixable: false,
        };
    }

    let contents = match std::fs::read_to_string(hook_path) {
        Ok(c) => c,
        Err(e) => {
            return SetupCheck {
                category,
                name,
                message: format!("Could not read hook {}: {e}", hook_path.display()),
                severity: SetupSeverity::Warning,
                fix_command: None,
                auto_fixable: false,
            };
        }
    };

    if contents.contains("LOOP_ALLOW_DESTRUCTIVE") {
        SetupCheck {
            category,
            name,
            message: format!("{} checks LOOP_ALLOW_DESTRUCTIVE — OK", hook_path.display()),
            severity: SetupSeverity::Pass,
            fix_command: None,
            auto_fixable: false,
        }
    } else {
        SetupCheck {
            category,
            name,
            message: format!(
                "{} does not check LOOP_ALLOW_DESTRUCTIVE — the loop will be blocked",
                hook_path.display()
            ),
            severity: SetupSeverity::Warning,
            fix_command: Some("task-mgr doctor --setup --auto-fix".to_string()),
            auto_fixable: true,
        }
    }
}

/// Check that each expected skill is installed in `global_dir` (typically
/// `~/.claude/commands/`).
///
/// # Returns
/// One `SetupCheck` per skill: `Pass` when present, `Warning` with a
/// copy-pasteable install command when absent.
pub fn check_skills_installed(global_dir: &Path, expected: &[&str]) -> Vec<SetupCheck> {
    expected
        .iter()
        .map(|name| {
            let skill_path = global_dir.join(format!("{name}.md"));
            if skill_path.exists() {
                SetupCheck {
                    category: SetupCategory::Skills,
                    name: format!("skill_{}", name.replace('-', "_")),
                    message: format!("Skill {name} is installed"),
                    severity: SetupSeverity::Pass,
                    fix_command: None,
                    auto_fixable: false,
                }
            } else {
                SetupCheck {
                    category: SetupCategory::Skills,
                    name: format!("skill_{}", name.replace('-', "_")),
                    message: format!("Skill {name} not found in {}", global_dir.display()),
                    severity: SetupSeverity::Warning,
                    fix_command: Some(format!("cp .claude/commands/{name}.md ~/.claude/commands/")),
                    auto_fixable: true,
                }
            }
        })
        .collect()
}

/// Check that `.task-mgr/config.json` exists in `db_dir`.
///
/// The project config file enables project-specific tool allowlists and
/// configuration for the loop engine.
///
/// # Returns
/// - `Pass` when `db_dir/config.json` exists
/// - `Warning` when it is absent
pub fn check_project_config(db_dir: &Path) -> SetupCheck {
    let config_path = db_dir.join("config.json");
    if config_path.exists() {
        SetupCheck {
            category: SetupCategory::ProjectConfig,
            name: "project_config".to_string(),
            message: format!("{} exists — OK", config_path.display()),
            severity: SetupSeverity::Pass,
            fix_command: None,
            auto_fixable: false,
        }
    } else {
        SetupCheck {
            category: SetupCategory::ProjectConfig,
            name: "project_config".to_string(),
            message: format!(
                "{} not found — project-specific tool configuration missing",
                config_path.display()
            ),
            severity: SetupSeverity::Warning,
            fix_command: Some("task-mgr doctor --setup --auto-fix".to_string()),
            auto_fixable: true,
        }
    }
}

/// Check that `CLAUDE.md` exists in `project_dir`.
///
/// `CLAUDE.md` provides project-specific instructions to Claude. Its absence
/// is informational — the loop will still work, but Claude won't have project
/// context.
///
/// # Returns
/// - `Pass` when `project_dir/CLAUDE.md` exists
/// - `Info` when it is absent
pub fn check_claude_md(project_dir: &Path) -> SetupCheck {
    let claude_md = project_dir.join("CLAUDE.md");
    if claude_md.exists() {
        SetupCheck {
            category: SetupCategory::Documentation,
            name: "claude_md".to_string(),
            message: "CLAUDE.md exists — OK".to_string(),
            severity: SetupSeverity::Pass,
            fix_command: None,
            auto_fixable: false,
        }
    } else {
        SetupCheck {
            category: SetupCategory::Documentation,
            name: "claude_md".to_string(),
            message: format!(
                "CLAUDE.md not found in {} — project instructions for Claude are missing",
                project_dir.display()
            ),
            severity: SetupSeverity::Info,
            fix_command: Some("task-mgr doctor --setup --auto-fix".to_string()),
            auto_fixable: true,
        }
    }
}

/// Run blocker-level pre-checks for loop startup on a new task list.
///
/// Checks only `defaultMode` and deny conflicts — the two settings that can
/// silently block all loop tool calls. Skips skills, project config, and
/// CLAUDE.md to keep startup latency well under 100ms.
///
/// # Arguments
/// - `global_dir`: Path to the Claude global config directory (typically
///   `~/.claude/`). The function reads `global_dir/settings.json`.
///
/// # Returns
/// All check results for `defaultMode` and deny conflicts. Callers should
/// filter for [`SetupSeverity::Blocker`] entries to decide whether to emit
/// a warning banner.
pub fn pre_check_loop_setup(global_dir: &Path) -> Vec<SetupCheck> {
    let settings_path = global_dir.join("settings.json");
    let mut checks = Vec::new();
    checks.push(check_default_mode(&settings_path));
    checks.extend(check_deny_conflicts(&settings_path));
    checks
}

// ─── helpers shared by all check functions ───────────────────────────────────

/// Write `contents` to `dir/filename` and return the full path.
#[cfg(test)]
fn write_fixture(dir: &std::path::Path, filename: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(filename);
    std::fs::write(&path, contents).expect("write_fixture failed");
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ─── check_default_mode ───────────────────────────────────────────────────

    /// `defaultMode: "default"` must produce a Blocker with a fix command.
    #[test]
    fn test_check_default_mode_fail_default_is_blocker() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"defaultMode":"default"}}"#,
        );

        let check = check_default_mode(&path);

        assert_eq!(
            check.severity,
            SetupSeverity::Blocker,
            "expected Blocker, got {:?}: {}",
            check.severity,
            check.message
        );
        assert!(
            check.fix_command.is_some(),
            "Blocker must include a fix command"
        );
        assert_eq!(check.name, "default_mode");
    }

    /// `defaultMode: "auto"` must produce a Pass.
    #[test]
    fn test_check_default_mode_pass_auto_is_pass() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"defaultMode":"auto"}}"#,
        );

        let check = check_default_mode(&path);

        assert_eq!(
            check.severity,
            SetupSeverity::Pass,
            "expected Pass, got {:?}: {}",
            check.severity,
            check.message
        );
    }

    /// Missing `settings.json` should not crash and should report Pass.
    #[test]
    fn test_check_default_mode_pass_missing_settings_is_pass() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let check = check_default_mode(&path);

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    /// An empty JSON object `{}` — no defaultMode key — must be Pass.
    #[test]
    fn test_check_default_mode_pass_empty_object_is_pass() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(dir.path(), "settings.json", "{}");

        let check = check_default_mode(&path);

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    /// Malformed JSON must produce a Warning, not a panic.
    #[test]
    fn test_check_default_mode_fail_malformed_json_is_warning() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(dir.path(), "settings.json", "{not valid json");

        let check = check_default_mode(&path);

        assert_eq!(check.severity, SetupSeverity::Warning);
    }

    /// `defaultMode: "acceptEdits"` (another safe mode) must be Pass.
    #[test]
    fn test_check_default_mode_pass_accept_edits_is_pass() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"defaultMode":"acceptEdits"}}"#,
        );

        let check = check_default_mode(&path);

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    // ─── check_deny_conflicts ─────────────────────────────────────────────────

    /// A deny rule that exactly matches a CODING_ALLOWED_TOOLS entry must be Blocker.
    #[test]
    fn test_check_deny_conflict_fail_matching_tool_is_blocker() {
        let dir = TempDir::new().unwrap();
        // "Bash(cargo:*)" is in CODING_ALLOWED_TOOLS
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":["Bash(cargo:*)"]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert!(!checks.is_empty(), "expected at least one conflict");
        assert!(
            checks.iter().any(|c| c.severity == SetupSeverity::Blocker),
            "matching deny rule must be Blocker"
        );
        // Must include a fix command so user knows how to remove the rule
        assert!(
            checks.iter().all(|c| c.fix_command.is_some()),
            "each conflict must have a fix command"
        );
    }

    /// A deny rule that does not match any allowed tool must produce no checks.
    #[test]
    fn test_check_deny_conflict_pass_non_matching_deny_is_empty() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":["SomeUnknownTool"]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert!(
            checks.is_empty(),
            "non-conflicting deny rule should produce no checks, got: {checks:?}"
        );
    }

    /// Deny rule 'Bash' (no parens) must be Blocker when any Bash(x:*) is allowed.
    #[test]
    fn test_check_deny_conflict_fail_bare_bash_name_is_blocker() {
        let dir = TempDir::new().unwrap();
        // "Bash" (no parens) would block all Bash(x:*) tools in CODING_ALLOWED_TOOLS
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":["Bash"]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert!(
            !checks.is_empty(),
            "bare 'Bash' deny rule must produce a conflict"
        );
        assert!(
            checks.iter().any(|c| c.severity == SetupSeverity::Blocker),
            "bare deny rule must be Blocker"
        );
        assert!(
            checks.iter().all(|c| c.fix_command.is_some()),
            "each conflict must have a fix command"
        );
    }

    /// Deny rule 'Bash(*)' must be Blocker when any Bash(x:*) is allowed.
    #[test]
    fn test_check_deny_conflict_fail_wildcard_bash_is_blocker() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":["Bash(*)"]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert!(
            !checks.is_empty(),
            "Bash(*) deny rule must produce a conflict"
        );
        assert!(
            checks.iter().any(|c| c.severity == SetupSeverity::Blocker),
            "Bash(*) deny rule must be Blocker"
        );
    }

    /// Deny rule 'FooTool' must NOT conflict with any CODING_ALLOWED_TOOLS entry.
    #[test]
    fn test_check_deny_conflict_pass_unrelated_bare_name_is_empty() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":["FooTool"]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert!(
            checks.is_empty(),
            "unrelated tool name should produce no conflicts, got: {checks:?}"
        );
    }

    /// Missing `settings.json` must return an empty Vec without panicking.
    #[test]
    fn test_check_deny_conflict_pass_missing_settings_is_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        let checks = check_deny_conflicts(&path);

        assert!(checks.is_empty());
    }

    /// Empty deny array must return an empty Vec.
    #[test]
    fn test_check_deny_conflict_pass_empty_deny_array_is_empty() {
        let dir = TempDir::new().unwrap();
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":[]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert!(checks.is_empty());
    }

    /// Multiple conflicting deny rules must each produce one Blocker.
    #[test]
    fn test_check_deny_conflict_fail_multiple_conflicts_each_produce_blocker() {
        let dir = TempDir::new().unwrap();
        // Both "Read" and "Bash(git:*)" are in CODING_ALLOWED_TOOLS
        let path = write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"deny":["Read","Bash(git:*)"]}}"#,
        );

        let checks = check_deny_conflicts(&path);

        assert_eq!(checks.len(), 2, "expected one Blocker per conflicting rule");
        assert!(checks.iter().all(|c| c.severity == SetupSeverity::Blocker));
    }

    // ─── check_hook_bypass ────────────────────────────────────────────────────

    /// Hook without `LOOP_ALLOW_DESTRUCTIVE` must produce Warning.
    #[test]
    fn test_check_hook_bypass_fail_missing_bypass_is_warning() {
        let dir = TempDir::new().unwrap();
        let hook = write_fixture(
            dir.path(),
            "guard-destructive.sh",
            "#!/bin/bash\necho 'guard hook'\n",
        );

        let check = check_hook_bypass(&hook);

        assert_eq!(
            check.severity,
            SetupSeverity::Warning,
            "hook without bypass must be Warning: {}",
            check.message
        );
        assert!(
            check.fix_command.is_some(),
            "Warning must include a fix command"
        );
    }

    /// Hook that contains `LOOP_ALLOW_DESTRUCTIVE` must produce Pass.
    #[test]
    fn test_check_hook_bypass_pass_bypass_present_is_pass() {
        let dir = TempDir::new().unwrap();
        let hook = write_fixture(
            dir.path(),
            "guard-destructive.sh",
            "#!/bin/bash\n[ -n \"$LOOP_ALLOW_DESTRUCTIVE\" ] && exit 0\necho 'guard'\n",
        );

        let check = check_hook_bypass(&hook);

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    /// Non-existent hook file must produce Pass (nothing to bypass).
    #[test]
    fn test_check_hook_bypass_pass_missing_hook_is_pass() {
        let dir = TempDir::new().unwrap();
        let hook = dir.path().join("guard-destructive.sh");

        let check = check_hook_bypass(&hook);

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    // ─── check_skills_installed ───────────────────────────────────────────────

    /// Missing skill must produce Warning with a copy-pasteable install command.
    #[test]
    fn test_check_skills_installed_fail_missing_skill_is_warning_with_command() {
        let dir = TempDir::new().unwrap();

        let checks = check_skills_installed(dir.path(), &["tm-apply", "tm-learn"]);

        assert_eq!(checks.len(), 2);
        for check in &checks {
            assert_eq!(
                check.severity,
                SetupSeverity::Warning,
                "missing skill must be Warning: {}",
                check.message
            );
            assert!(
                check.fix_command.is_some(),
                "missing skill must include install command"
            );
        }
    }

    /// Present skill must produce Pass.
    #[test]
    fn test_check_skills_installed_pass_present_skill_is_pass() {
        let dir = TempDir::new().unwrap();
        write_fixture(dir.path(), "tm-apply.md", "# tm-apply");

        let checks = check_skills_installed(dir.path(), &["tm-apply"]);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].severity, SetupSeverity::Pass);
    }

    /// Mixed: some present, some missing — each reported independently.
    #[test]
    fn test_check_skills_installed_mixed_some_pass_some_warning() {
        let dir = TempDir::new().unwrap();
        write_fixture(dir.path(), "tm-apply.md", "# tm-apply");
        // tm-learn is absent

        let checks = check_skills_installed(dir.path(), &["tm-apply", "tm-learn"]);

        assert_eq!(checks.len(), 2);
        let pass_count = checks
            .iter()
            .filter(|c| c.severity == SetupSeverity::Pass)
            .count();
        let warn_count = checks
            .iter()
            .filter(|c| c.severity == SetupSeverity::Warning)
            .count();
        assert_eq!(pass_count, 1, "tm-apply should pass");
        assert_eq!(warn_count, 1, "tm-learn should warn");
    }

    /// Empty expected list must return empty Vec.
    #[test]
    fn test_check_skills_installed_pass_empty_expected_is_empty() {
        let dir = TempDir::new().unwrap();

        let checks = check_skills_installed(dir.path(), &[]);

        assert!(checks.is_empty());
    }

    // ─── check_project_config ─────────────────────────────────────────────────

    /// Missing `config.json` in `db_dir` must produce Warning.
    #[test]
    fn test_check_project_config_fail_missing_is_warning() {
        let dir = TempDir::new().unwrap();

        let check = check_project_config(dir.path());

        assert_eq!(
            check.severity,
            SetupSeverity::Warning,
            "missing config.json must be Warning: {}",
            check.message
        );
        assert!(check.fix_command.is_some());
    }

    /// Present `config.json` must produce Pass.
    #[test]
    fn test_check_project_config_pass_present_is_pass() {
        let dir = TempDir::new().unwrap();
        write_fixture(dir.path(), "config.json", "{}");

        let check = check_project_config(dir.path());

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    // ─── check_claude_md ──────────────────────────────────────────────────────

    /// Missing `CLAUDE.md` must produce Info (not Blocker or Warning — it's
    /// informational; the loop still runs without it).
    #[test]
    fn test_check_claude_md_fail_missing_is_info() {
        let dir = TempDir::new().unwrap();

        let check = check_claude_md(dir.path());

        assert_eq!(
            check.severity,
            SetupSeverity::Info,
            "missing CLAUDE.md must be Info: {}",
            check.message
        );
    }

    /// Present `CLAUDE.md` must produce Pass.
    #[test]
    fn test_check_claude_md_pass_present_is_pass() {
        let dir = TempDir::new().unwrap();
        write_fixture(dir.path(), "CLAUDE.md", "# project");

        let check = check_claude_md(dir.path());

        assert_eq!(check.severity, SetupSeverity::Pass);
    }

    // ─── all checks pass on correctly configured project ─────────────────────

    /// A fully-configured project must produce Pass on every check.
    ///
    /// Fixture layout:
    /// ```
    /// home_dir/
    ///   .claude/
    ///     settings.json         ← defaultMode=auto, deny=[]
    ///     hooks/
    ///       guard-destructive.sh  ← contains LOOP_ALLOW_DESTRUCTIVE
    ///     commands/
    ///       tm-apply.md, tm-learn.md, ...
    /// project_dir/
    ///   .task-mgr/
    ///     config.json
    ///   CLAUDE.md
    /// ```
    #[test]
    fn test_all_checks_pass_on_correctly_configured_project() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        // ~/.claude/settings.json — safe mode, no deny conflicts
        let claude_dir = home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        write_fixture(
            &claude_dir,
            "settings.json",
            r#"{"permissions":{"defaultMode":"auto","deny":[]}}"#,
        );

        // Hook with bypass
        let hooks_dir = claude_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        write_fixture(
            &hooks_dir,
            "guard-destructive.sh",
            "#!/bin/bash\n[ -n \"$LOOP_ALLOW_DESTRUCTIVE\" ] && exit 0\n",
        );

        // All skills installed
        let commands_dir = claude_dir.join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        for skill in EXPECTED_SKILLS {
            write_fixture(&commands_dir, &format!("{skill}.md"), "# skill");
        }

        // Project config and CLAUDE.md
        let db_dir = project.path().join(".task-mgr");
        std::fs::create_dir_all(&db_dir).unwrap();
        write_fixture(&db_dir, "config.json", "{}");
        write_fixture(project.path(), "CLAUDE.md", "# Project instructions");

        // ── assert all Pass ──

        let settings_path = claude_dir.join("settings.json");
        let hook_path = hooks_dir.join("guard-destructive.sh");

        let mode_check = check_default_mode(&settings_path);
        assert_eq!(
            mode_check.severity,
            SetupSeverity::Pass,
            "defaultMode: {}",
            mode_check.message
        );

        let deny_checks = check_deny_conflicts(&settings_path);
        assert!(
            deny_checks.is_empty(),
            "no deny conflicts expected, got: {deny_checks:?}"
        );

        let hook_check = check_hook_bypass(&hook_path);
        assert_eq!(
            hook_check.severity,
            SetupSeverity::Pass,
            "hook bypass: {}",
            hook_check.message
        );

        let skill_checks = check_skills_installed(&commands_dir, EXPECTED_SKILLS);
        for check in &skill_checks {
            assert_eq!(
                check.severity,
                SetupSeverity::Pass,
                "skill {}: {}",
                check.name,
                check.message
            );
        }

        let config_check = check_project_config(&db_dir);
        assert_eq!(
            config_check.severity,
            SetupSeverity::Pass,
            "project config: {}",
            config_check.message
        );

        let claude_md_check = check_claude_md(project.path());
        assert_eq!(
            claude_md_check.severity,
            SetupSeverity::Pass,
            "CLAUDE.md: {}",
            claude_md_check.message
        );
    }

    // ─── pre_check_loop_setup ─────────────────────────────────────────────────

    /// No settings.json → all Pass, no Blockers.
    #[test]
    fn test_pre_check_loop_setup_pass_no_settings() {
        let dir = TempDir::new().unwrap();

        let checks = pre_check_loop_setup(dir.path());

        assert!(
            checks.iter().all(|c| c.severity != SetupSeverity::Blocker),
            "missing settings.json should not produce Blockers: {checks:?}"
        );
    }

    /// `defaultMode: "default"` → contains a Blocker.
    #[test]
    fn test_pre_check_loop_setup_fail_default_mode_produces_blocker() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"defaultMode":"default"}}"#,
        );

        let checks = pre_check_loop_setup(dir.path());

        assert!(
            checks.iter().any(|c| c.severity == SetupSeverity::Blocker),
            "defaultMode=default must produce at least one Blocker: {checks:?}"
        );
    }

    /// A deny conflict → contains a Blocker.
    #[test]
    fn test_pre_check_loop_setup_fail_deny_conflict_produces_blocker() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"defaultMode":"auto","deny":["Bash(cargo:*)"]}} "#,
        );

        let checks = pre_check_loop_setup(dir.path());

        assert!(
            checks.iter().any(|c| c.severity == SetupSeverity::Blocker),
            "deny conflict must produce at least one Blocker: {checks:?}"
        );
    }

    /// Clean settings → no Blockers, includes a Pass for defaultMode.
    #[test]
    fn test_pre_check_loop_setup_pass_clean_settings_no_blockers() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            dir.path(),
            "settings.json",
            r#"{"permissions":{"defaultMode":"auto","deny":[]}}"#,
        );

        let checks = pre_check_loop_setup(dir.path());

        assert!(
            !checks.is_empty(),
            "should return at least one check result"
        );
        assert!(
            checks.iter().all(|c| c.severity != SetupSeverity::Blocker),
            "clean settings must have no Blockers: {checks:?}"
        );
    }
}
