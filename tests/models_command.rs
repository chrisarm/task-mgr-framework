//! Integration tests for the provider-first `task-mgr models` verb set (FR-009).
//!
//! Runs the real CLI binary via `assert_cmd`. Uses an isolated `$HOME` /
//! `XDG_*` so tests don't touch the developer's real config, and forces
//! `TASK_MGR_USE_API` off so no HTTP requests are ever made.

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use std::path::PathBuf;
use tempfile::TempDir;

use task_mgr::loop_engine::model::{FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};

/// A tempdir for `--dir` plus a second tempdir acting as `$HOME`.
struct Sandbox {
    db_dir: TempDir,
    home: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            db_dir: TempDir::new().unwrap(),
            home: TempDir::new().unwrap(),
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(cargo_bin("task-mgr"));
        cmd.env("HOME", self.home.path());
        cmd.env_remove("XDG_CONFIG_HOME");
        cmd.env_remove("XDG_CACHE_HOME");
        cmd.env_remove("TASK_MGR_USE_API");
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.args(["--dir", self.db_dir.path().to_str().unwrap()]);
        cmd
    }

    fn project_config(&self) -> PathBuf {
        self.db_dir.path().join("config.json")
    }

    fn write_config(&self, contents: &str) {
        std::fs::write(self.project_config(), contents).unwrap();
    }

    fn read_config(&self) -> String {
        std::fs::read_to_string(self.project_config()).unwrap()
    }

    fn stdout_of(&self, args: &[&str]) -> String {
        let out = self
            .cmd()
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    }
}

// ---- init ----------------------------------------------------------------

#[test]
fn init_writes_block_and_show_renders_it() {
    let sb = Sandbox::new();
    sb.cmd().args(["models", "init"]).assert().success();
    let raw = sb.read_config();
    assert!(raw.contains("\"models\""), "models block written:\n{raw}");
    assert!(raw.contains("\"anchor\""), "anchor key written:\n{raw}");
    assert!(raw.contains("standard"), "default anchor standard:\n{raw}");

    let show = sb.stdout_of(&["models", "show"]);
    assert!(show.contains("primaryProvider: claude"), "{show}");
    assert!(show.contains("Codex pinning is route-only"), "{show}");
}

#[test]
fn init_dry_run_writes_nothing() {
    let sb = Sandbox::new();
    sb.write_config(r#"{"version":1,"defaultModel":"x"}"#);
    let original = sb.read_config();
    let out = sb.stdout_of(&["models", "init", "--dry-run"]);
    assert!(out.contains("dry-run"), "dry-run banner:\n{out}");
    assert!(out.contains("defaultModel"), "shows legacy half:\n{out}");
    assert_eq!(sb.read_config(), original, "dry-run must not write");
}

#[test]
fn init_force_replace_legacy_deletes_keys() {
    let sb = Sandbox::new();
    sb.write_config(
        r#"{"version":1,"defaultModel":"a","reviewModel":"b","primaryRunner":{},
            "fallbackRunner":{"enabled":true},"additionalAllowedTools":["Bash(docker:*)"]}"#,
    );
    sb.cmd()
        .args(["models", "init", "--force-replace-legacy"])
        .assert()
        .success();
    let raw = sb.read_config();
    for key in [
        "defaultModel",
        "reviewModel",
        "primaryRunner",
        "fallbackRunner",
    ] {
        assert!(!raw.contains(key), "{key} must be deleted:\n{raw}");
    }
    assert!(
        raw.contains("additionalAllowedTools"),
        "unknown key preserved:\n{raw}"
    );
}

// ---- show ----------------------------------------------------------------

#[test]
fn show_default_anchor_maps_difficulties_to_models() {
    let sb = Sandbox::new();
    let show = sb.stdout_of(&["models", "show"]);
    assert!(show.contains(SONNET_MODEL), "low→sonnet:\n{show}");
    assert!(show.contains(OPUS_MODEL), "medium→opus:\n{show}");
    assert!(
        show.contains(FABLE_MODEL),
        "high→fable + crash escalation:\n{show}"
    );
    assert!(show.contains("Crash escalation"), "{show}");
}

#[test]
fn set_anchor_shifts_difficulty_window() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-anchor", "cost-efficient"])
        .assert()
        .success();
    let show = sb.stdout_of(&["models", "show"]);
    assert!(show.contains("anchor:          cost-efficient"), "{show}");
    assert!(
        show.contains(HAIKU_MODEL),
        "low→haiku at anchor cost-efficient:\n{show}"
    );
}

#[test]
fn set_anchor_typo_is_config_error() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-anchor", "fronteir"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("CONFIG ERROR"))
        .stderr(predicates::str::contains("frontier"));
}

// ---- list ----------------------------------------------------------------

#[test]
fn list_renders_provider_ladders() {
    let sb = Sandbox::new();
    let out = sb.stdout_of(&["models", "list"]);
    assert!(out.contains("Claude"), "{out}");
    assert!(out.contains(OPUS_MODEL), "{out}");
    assert!(out.contains(SONNET_MODEL), "{out}");
    assert!(out.contains("Grok"), "{out}");
}

// ---- route ---------------------------------------------------------------

#[test]
fn route_adds_byidprefix_route() {
    let sb = Sandbox::new();
    sb.cmd()
        .args([
            "models",
            "route",
            "REVIEW-",
            "--provider",
            "claude",
            "--tier",
            "frontier",
        ])
        .assert()
        .success();
    let show = sb.stdout_of(&["models", "show"]);
    assert!(
        show.contains("REVIEW-"),
        "route must appear in show:\n{show}"
    );
    assert!(
        show.contains("frontier"),
        "forced tier must appear:\n{show}"
    );
}

// ---- removed verbs + legacy guard ----------------------------------------

#[test]
fn removed_set_default_verb_prints_replacement_hint() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-default", OPUS_MODEL])
        .assert()
        .failure()
        .stderr(predicates::str::contains("set-anchor"));
}

#[test]
fn mutating_verb_on_legacy_config_hard_errors() {
    let sb = Sandbox::new();
    sb.write_config(r#"{"version":1,"defaultModel":"x","fallbackRunner":{"enabled":true}}"#);
    sb.cmd()
        .args(["models", "set-anchor", "standard"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("defaultModel"))
        .stderr(predicates::str::contains(
            "models init --force-replace-legacy",
        ));
}

// ---- enable (end-to-end through the probe) -------------------------------

#[cfg(unix)]
#[test]
fn enable_provider_through_cli_with_temp_binaries() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let exe = sb.home.path().join("fake-bin");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&exe).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&exe, perms).unwrap();
    let exe = exe.to_str().unwrap();

    sb.cmd()
        .env("CLAUDE_BINARY", exe)
        .env("GROK_BINARY", exe)
        .args(["models", "enable", "grok"])
        .assert()
        .success();

    let show = sb.stdout_of(&["models", "show"]);
    // Grok now renders as enabled in the ladder block.
    assert!(
        show.contains("Grok (enabled)"),
        "grok must be enabled:\n{show}"
    );
}
