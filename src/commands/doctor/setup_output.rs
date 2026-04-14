//! Output types for the `doctor --setup` command.
//!
//! Contains:
//! - `SetupSeverity` enum for classifying check findings
//! - `SetupCategory` enum for grouping related checks
//! - `SetupCheck` struct for individual check results
//! - `SetupAuditResult` for the aggregated audit outcome

use serde::{Deserialize, Serialize};

/// Severity level of a setup check result.
///
/// Ordered so that `Blocker > Warning > Info > Pass` — useful for sorting
/// or finding the worst severity across a list of checks.
///
/// The declaration order (Pass = 0, Blocker = 3) means the derived `Ord`
/// implementation satisfies `Blocker > Warning > Info > Pass`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SetupSeverity {
    /// Check passed — no issue detected.
    Pass,
    /// Informational finding — no action required but worth knowing.
    Info,
    /// Non-critical issue that may cause problems in some situations.
    Warning,
    /// Critical misconfiguration that blocks correct loop operation.
    Blocker,
}

/// Category of a setup check, used to group related checks in output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupCategory {
    /// CLAUDE.md presence and content
    Documentation,
    /// `PreToolUse` / `PostToolUse` hooks
    Hooks,
    /// Claude Code permissions (allow/deny lists, defaultMode)
    Permissions,
    /// Project config (`.task-mgr/config.json`)
    ProjectConfig,
    /// Installed skills in `~/.claude/commands/`
    Skills,
}

/// Result of a single setup check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupCheck {
    /// Category this check belongs to
    pub category: SetupCategory,
    /// Short machine-readable identifier for this check
    pub name: String,
    /// Human-readable description of the finding
    pub message: String,
    /// Severity of the finding
    pub severity: SetupSeverity,
    /// A copy-pasteable shell command that fixes the issue, if one exists
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_command: Option<String>,
    /// Whether the issue can be auto-fixed by `doctor --setup --auto-fix`
    pub auto_fixable: bool,
}

/// Result of an auto-fix operation applied to a setup issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupFix {
    /// Short name identifying what was fixed (mirrors `SetupCheck::name`)
    pub name: String,
    /// Human-readable description of the action taken
    pub action: String,
    /// Whether the fix succeeded
    pub success: bool,
}

/// Aggregated result of running all setup checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupAuditResult {
    /// All check results, in the order they were run
    pub checks: Vec<SetupCheck>,
    /// Fixes applied during an `--auto-fix` run. Empty when not in auto-fix mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<SetupFix>,
}

impl SetupAuditResult {
    /// Create a new audit result from a list of checks (no fixes applied).
    pub fn new(checks: Vec<SetupCheck>) -> Self {
        Self {
            checks,
            fixes: Vec::new(),
        }
    }

    /// Create a new audit result with both checks and applied fixes.
    pub fn new_with_fixes(checks: Vec<SetupCheck>, fixes: Vec<SetupFix>) -> Self {
        Self { checks, fixes }
    }

    /// Number of checks with `Blocker` severity.
    pub fn blocker_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.severity == SetupSeverity::Blocker)
            .count()
    }

    /// Number of checks with `Warning` severity.
    pub fn warning_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.severity == SetupSeverity::Warning)
            .count()
    }

    /// Number of checks with `Pass` severity.
    pub fn passing_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.severity == SetupSeverity::Pass)
            .count()
    }

    /// Returns `true` if any check has `Blocker` severity.
    pub fn has_blockers(&self) -> bool {
        self.blocker_count() > 0
    }
}

// ANSI color codes for terminal output.
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

/// Format a `SetupAuditResult` as human-readable colored text.
///
/// Colors: red = Blocker, yellow = Warning, cyan = Info, green = Pass.
#[must_use]
pub fn format_setup_text(result: &SetupAuditResult) -> String {
    let mut out = String::new();

    out.push_str(&format!("{BOLD}=== Setup Audit ==={RESET}\n\n"));

    if result.checks.is_empty() {
        out.push_str(&format!("{GREEN}✓ No checks registered.{RESET}\n"));
        return out;
    }

    for check in &result.checks {
        let (color, label) = match check.severity {
            SetupSeverity::Blocker => (RED, "BLOCKER"),
            SetupSeverity::Warning => (YELLOW, "WARNING"),
            SetupSeverity::Info => (CYAN, "INFO   "),
            SetupSeverity::Pass => (GREEN, "PASS   "),
        };
        out.push_str(&format!(
            "{color}[{label}]{RESET} {}: {}\n",
            check.name, check.message
        ));
        if let Some(cmd) = &check.fix_command {
            out.push_str(&format!("         Fix: {cmd}\n"));
        }
    }

    // Show applied fixes when present (auto-fix run).
    if !result.fixes.is_empty() {
        out.push_str(&format!("{BOLD}=== Applied Fixes ==={RESET}\n\n"));
        for fix in &result.fixes {
            let (color, label) = if fix.success {
                (GREEN, "FIXED  ")
            } else {
                (RED, "FAILED ")
            };
            out.push_str(&format!(
                "{color}[{label}]{RESET} {}: {}\n",
                fix.name, fix.action
            ));
        }
        out.push('\n');
    }

    let b = result.blocker_count();
    let w = result.warning_count();
    let p = result.passing_count();
    let total = result.checks.len();

    if b > 0 {
        out.push_str(&format!(
            "{RED}{BOLD}{b} blocker(s){RESET}, {w} warning(s), {p} passing / {total} total\n"
        ));
        out.push_str("Run `task-mgr doctor --setup --auto-fix` to repair auto-fixable issues.\n");
    } else if w > 0 {
        out.push_str(&format!(
            "{YELLOW}{BOLD}{w} warning(s){RESET}, {p} passing / {total} total\n"
        ));
    } else {
        out.push_str(&format!(
            "{GREEN}{BOLD}All {total} check(s) passed.{RESET}\n"
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── helpers ─────────────────────────────────────────────────────────────

    fn make_check(severity: SetupSeverity) -> SetupCheck {
        SetupCheck {
            category: SetupCategory::Permissions,
            name: "test_check".to_string(),
            message: "test message".to_string(),
            severity,
            fix_command: None,
            auto_fixable: false,
        }
    }

    // ─── SetupSeverity ordering ───────────────────────────────────────────────

    #[test]
    fn test_severity_blocker_gt_warning() {
        assert!(SetupSeverity::Blocker > SetupSeverity::Warning);
    }

    #[test]
    fn test_severity_warning_gt_info() {
        assert!(SetupSeverity::Warning > SetupSeverity::Info);
    }

    #[test]
    fn test_severity_info_gt_pass() {
        assert!(SetupSeverity::Info > SetupSeverity::Pass);
    }

    #[test]
    fn test_severity_blocker_is_max_of_all() {
        let severities = [
            SetupSeverity::Pass,
            SetupSeverity::Info,
            SetupSeverity::Warning,
            SetupSeverity::Blocker,
        ];
        assert_eq!(severities.iter().max().unwrap(), &SetupSeverity::Blocker);
    }

    // ─── SetupCheck JSON serialization ────────────────────────────────────────

    #[test]
    fn test_setup_check_serializes_all_fields_when_fix_command_present() {
        let check = SetupCheck {
            category: SetupCategory::Permissions,
            name: "default_mode_check".to_string(),
            message: "defaultMode should not be 'default'".to_string(),
            severity: SetupSeverity::Blocker,
            fix_command: Some(
                r#"jq '.permissions.defaultMode = "acceptEdits"' ~/.claude/settings.json"#
                    .to_string(),
            ),
            auto_fixable: false,
        };

        let json = serde_json::to_string(&check).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["name"], "default_mode_check");
        assert_eq!(v["message"], "defaultMode should not be 'default'");
        assert_eq!(v["severity"], "blocker");
        assert_eq!(v["category"], "permissions");
        assert!(
            v["fix_command"].is_string(),
            "fix_command should be a string"
        );
    }

    /// Known-bad discriminator: fix_command=None must not appear in JSON output.
    #[test]
    fn test_setup_check_serializes_without_fix_command_when_none() {
        let check = SetupCheck {
            category: SetupCategory::ProjectConfig,
            name: "config_present".to_string(),
            message: "Project config found".to_string(),
            severity: SetupSeverity::Pass,
            fix_command: None,
            auto_fixable: false,
        };

        let json = serde_json::to_string(&check).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // skip_serializing_if = "Option::is_none" means the key must be absent
        assert!(
            v.get("fix_command").is_none(),
            "fix_command key must be absent when None, got: {json}"
        );
        assert_eq!(v["severity"], "pass");
        assert_eq!(v["category"], "project_config");
    }

    #[test]
    fn test_setup_check_all_categories_serialize_snake_case() {
        let categories = [
            (SetupCategory::Documentation, "documentation"),
            (SetupCategory::Hooks, "hooks"),
            (SetupCategory::Permissions, "permissions"),
            (SetupCategory::ProjectConfig, "project_config"),
            (SetupCategory::Skills, "skills"),
        ];
        for (cat, expected) in categories {
            let json = serde_json::to_string(&cat).unwrap();
            assert_eq!(
                json,
                format!("\"{expected}\""),
                "Category {cat:?} should serialize as {expected:?}"
            );
        }
    }

    // ─── SetupAuditResult counting ────────────────────────────────────────────

    #[test]
    fn test_audit_result_counts_blockers_warnings_passing() {
        let result = SetupAuditResult::new(vec![
            make_check(SetupSeverity::Blocker),
            make_check(SetupSeverity::Blocker),
            make_check(SetupSeverity::Warning),
            make_check(SetupSeverity::Info),
            make_check(SetupSeverity::Pass),
        ]);

        assert_eq!(result.blocker_count(), 2);
        assert_eq!(result.warning_count(), 1);
        assert_eq!(result.passing_count(), 1);
    }

    #[test]
    fn test_audit_result_has_blockers_true_when_blocker_present() {
        let result = SetupAuditResult::new(vec![
            make_check(SetupSeverity::Pass),
            make_check(SetupSeverity::Blocker),
        ]);
        assert!(result.has_blockers());
    }

    #[test]
    fn test_audit_result_has_blockers_false_with_only_warnings() {
        let result = SetupAuditResult::new(vec![
            make_check(SetupSeverity::Warning),
            make_check(SetupSeverity::Info),
            make_check(SetupSeverity::Pass),
        ]);
        assert!(!result.has_blockers());
    }

    /// Test: empty audit returns all-passing result (zero counts everywhere).
    #[test]
    fn test_empty_audit_returns_zero_counts() {
        let result = SetupAuditResult::new(vec![]);

        assert_eq!(result.blocker_count(), 0);
        assert_eq!(result.warning_count(), 0);
        assert_eq!(result.passing_count(), 0);
        assert!(!result.has_blockers());
        assert!(result.checks.is_empty());
    }

    // ─── SetupFix ─────────────────────────────────────────────────────────────

    #[test]
    fn test_setup_fix_serializes_correctly() {
        let fix = SetupFix {
            name: "install_skill_tm_apply".to_string(),
            action: "Copied tm-apply.md to ~/.claude/commands/".to_string(),
            success: true,
        };
        let json = serde_json::to_string(&fix).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["name"], "install_skill_tm_apply");
        assert_eq!(v["action"], "Copied tm-apply.md to ~/.claude/commands/");
        assert_eq!(v["success"], true);
    }

    #[test]
    fn test_setup_fix_failed_serializes_success_false() {
        let fix = SetupFix {
            name: "patch_hook".to_string(),
            action: "Patched guard-destructive.sh".to_string(),
            success: false,
        };
        let json = serde_json::to_string(&fix).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["success"], false);
    }

    // ─── auto_fixable field ───────────────────────────────────────────────────

    #[test]
    fn test_setup_check_auto_fixable_serializes() {
        let check = SetupCheck {
            category: SetupCategory::Skills,
            name: "skill_missing".to_string(),
            message: "tm-apply skill not found".to_string(),
            severity: SetupSeverity::Warning,
            fix_command: Some("cp tm-apply.md ~/.claude/commands/".to_string()),
            auto_fixable: true,
        };
        let json = serde_json::to_string(&check).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["auto_fixable"], true);
    }

    // ─── format_setup_text ────────────────────────────────────────────────────

    #[test]
    fn test_format_setup_text_empty_checks() {
        let result = SetupAuditResult::new(vec![]);
        let text = format_setup_text(&result);
        assert!(text.contains("No checks registered"), "got: {text}");
    }

    #[test]
    fn test_format_setup_text_all_pass() {
        let result = SetupAuditResult::new(vec![
            make_check(SetupSeverity::Pass),
            make_check(SetupSeverity::Pass),
        ]);
        let text = format_setup_text(&result);
        assert!(text.contains("PASS"), "got: {text}");
        assert!(text.contains("All 2 check(s) passed"), "got: {text}");
    }

    #[test]
    fn test_format_setup_text_blocker_shows_summary() {
        let result = SetupAuditResult::new(vec![
            make_check(SetupSeverity::Blocker),
            make_check(SetupSeverity::Pass),
        ]);
        let text = format_setup_text(&result);
        assert!(text.contains("BLOCKER"), "got: {text}");
        assert!(text.contains("1 blocker(s)"), "got: {text}");
        assert!(text.contains("--auto-fix"), "got: {text}");
    }

    #[test]
    fn test_format_setup_text_warning_only_shows_warning_summary() {
        let result = SetupAuditResult::new(vec![
            make_check(SetupSeverity::Warning),
            make_check(SetupSeverity::Pass),
        ]);
        let text = format_setup_text(&result);
        assert!(text.contains("WARNING"), "got: {text}");
        assert!(text.contains("1 warning(s)"), "got: {text}");
        assert!(
            !text.contains("blocker"),
            "unexpected blocker mention: {text}"
        );
    }

    #[test]
    fn test_format_setup_text_includes_fix_command() {
        let check = SetupCheck {
            category: SetupCategory::Permissions,
            name: "default_mode".to_string(),
            message: "defaultMode is 'default'".to_string(),
            severity: SetupSeverity::Blocker,
            fix_command: Some("jq '.permissions.defaultMode = \"auto\"' s.json".to_string()),
            auto_fixable: false,
        };
        let result = SetupAuditResult::new(vec![check]);
        let text = format_setup_text(&result);
        assert!(text.contains("Fix:"), "got: {text}");
        assert!(text.contains("defaultMode"), "got: {text}");
    }

    // ─── SetupAuditResult JSON round-trip ─────────────────────────────────────

    /// Serialize a `SetupAuditResult` to JSON and deserialize it back.
    /// The round-tripped value must equal the original on every field.
    #[test]
    fn test_setup_audit_result_json_round_trip() {
        let original = SetupAuditResult::new_with_fixes(
            vec![
                SetupCheck {
                    category: SetupCategory::Permissions,
                    name: "default_mode".to_string(),
                    message: "permissions.defaultMode is \"auto\" — OK".to_string(),
                    severity: SetupSeverity::Pass,
                    fix_command: None,
                    auto_fixable: false,
                },
                SetupCheck {
                    category: SetupCategory::Skills,
                    name: "skill_tm_apply".to_string(),
                    message: "Skill tm-apply not found".to_string(),
                    severity: SetupSeverity::Warning,
                    fix_command: Some(
                        "cp .claude/commands/tm-apply.md ~/.claude/commands/".to_string(),
                    ),
                    auto_fixable: true,
                },
                SetupCheck {
                    category: SetupCategory::Hooks,
                    name: "hook_bypass".to_string(),
                    message: "hook_bypass — BLOCKER".to_string(),
                    severity: SetupSeverity::Blocker,
                    fix_command: Some("task-mgr doctor --setup --auto-fix".to_string()),
                    auto_fixable: true,
                },
                SetupCheck {
                    category: SetupCategory::Documentation,
                    name: "claude_md".to_string(),
                    message: "CLAUDE.md not found — info".to_string(),
                    severity: SetupSeverity::Info,
                    fix_command: Some("task-mgr doctor --setup --auto-fix".to_string()),
                    auto_fixable: true,
                },
            ],
            vec![
                SetupFix {
                    name: "install_skill_tm_apply".to_string(),
                    action: "Copied tm-apply.md to ~/.claude/commands/".to_string(),
                    success: true,
                },
                SetupFix {
                    name: "generate_claude_md".to_string(),
                    action: "Generated template CLAUDE.md".to_string(),
                    success: false,
                },
            ],
        );

        let json = serde_json::to_string(&original).expect("serialization must not fail");
        let restored: SetupAuditResult =
            serde_json::from_str(&json).expect("deserialization must not fail");

        assert_eq!(
            original.checks.len(),
            restored.checks.len(),
            "checks length must round-trip"
        );
        assert_eq!(
            original.fixes.len(),
            restored.fixes.len(),
            "fixes length must round-trip"
        );

        for (orig, rest) in original.checks.iter().zip(restored.checks.iter()) {
            assert_eq!(orig.name, rest.name, "check name must round-trip");
            assert_eq!(
                orig.severity, rest.severity,
                "check severity must round-trip"
            );
            assert_eq!(
                orig.category, rest.category,
                "check category must round-trip"
            );
            assert_eq!(orig.message, rest.message, "check message must round-trip");
            assert_eq!(
                orig.fix_command, rest.fix_command,
                "fix_command must round-trip"
            );
            assert_eq!(
                orig.auto_fixable, rest.auto_fixable,
                "auto_fixable must round-trip"
            );
        }

        for (orig, rest) in original.fixes.iter().zip(restored.fixes.iter()) {
            assert_eq!(orig.name, rest.name, "fix name must round-trip");
            assert_eq!(orig.action, rest.action, "fix action must round-trip");
            assert_eq!(orig.success, rest.success, "fix success must round-trip");
        }

        assert_eq!(original.blocker_count(), restored.blocker_count());
        assert_eq!(original.warning_count(), restored.warning_count());
    }

    /// A `SetupAuditResult` with no fixes must round-trip without the `fixes` key
    /// being present in JSON (due to `skip_serializing_if = "Vec::is_empty"`),
    /// but deserialization must still reconstruct an empty `fixes` vec.
    #[test]
    fn test_setup_audit_result_no_fixes_round_trip() {
        let original = SetupAuditResult::new(vec![make_check(SetupSeverity::Pass)]);

        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("fixes").is_none(),
            "fixes key must be absent when empty, got: {json}"
        );

        let restored: SetupAuditResult = serde_json::from_str(&json).unwrap();
        assert!(
            restored.fixes.is_empty(),
            "deserialized fixes must be empty vec"
        );
        assert_eq!(restored.checks.len(), 1);
    }
}
