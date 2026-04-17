//! Integration tests for `task-mgr models` subcommands.
//!
//! Runs the real CLI binary via `assert_cmd`. Uses an isolated
//! `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` so tests don't touch the developer's
//! real user config. The `TASK_MGR_USE_API` env var is forced off in every
//! test so no HTTP requests are ever made.

#![allow(deprecated)]

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use std::path::PathBuf;
use tempfile::TempDir;

use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};

/// Handy pair: a tempdir for `--dir` and a second tempdir acting as `$HOME`
/// so user-config writes land in an isolated location.
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
        // Isolate per-user config + cache so tests don't clobber each other.
        cmd.env("HOME", self.home.path());
        cmd.env_remove("XDG_CONFIG_HOME");
        cmd.env_remove("XDG_CACHE_HOME");
        // Hard-off the remote fetch so nothing ever touches the network.
        cmd.env_remove("TASK_MGR_USE_API");
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.args(["--dir", self.db_dir.path().to_str().unwrap()]);
        cmd
    }

    fn project_config(&self) -> PathBuf {
        self.db_dir.path().join("config.json")
    }
}

#[test]
fn list_offline_prints_built_in_models() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["models", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains(OPUS_MODEL),
        "output should contain opus id:\n{s}"
    );
    assert!(s.contains(SONNET_MODEL));
    assert!(s.contains(HAIKU_MODEL));
    assert!(s.contains("Difficulty"), "effort table expected:\n{s}");
}

#[test]
fn list_remote_without_opt_in_falls_back_silently() {
    // With TASK_MGR_USE_API unset (Sandbox default), --remote must not attempt
    // a live fetch. We can't strictly prove no network call happened from here,
    // but the built-in list header identifies the offline path.
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["models", "list", "--remote"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("built-in list"), "expected offline path: {s}");
}

#[test]
fn set_default_user_round_trips_via_show() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-default", OPUS_MODEL])
        .assert()
        .success();
    let out = sb
        .cmd()
        .args(["models", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains(OPUS_MODEL));
    assert!(s.contains("source: user"), "expected source=user:\n{s}");
}

#[test]
fn set_default_project_beats_user_in_show() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-default", HAIKU_MODEL])
        .assert()
        .success();
    sb.cmd()
        .args(["models", "set-default", SONNET_MODEL, "--project"])
        .assert()
        .success();
    let out = sb
        .cmd()
        .args(["models", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains(SONNET_MODEL));
    assert!(
        s.contains("source: project"),
        "project default must win over user:\n{s}"
    );
}

#[test]
fn unset_default_clears_user_only_by_default() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-default", OPUS_MODEL])
        .assert()
        .success();
    sb.cmd()
        .args(["models", "set-default", HAIKU_MODEL, "--project"])
        .assert()
        .success();

    // Unset (user-level). Project default should still show.
    sb.cmd()
        .args(["models", "unset-default"])
        .assert()
        .success();
    let out = sb
        .cmd()
        .args(["models", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains(HAIKU_MODEL));
    assert!(s.contains("source: project"));
}

#[test]
fn unset_default_project_clears_project_config() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["models", "set-default", SONNET_MODEL, "--project"])
        .assert()
        .success();
    let raw_before = std::fs::read_to_string(sb.project_config()).unwrap();
    assert!(raw_before.contains(SONNET_MODEL));

    sb.cmd()
        .args(["models", "unset-default", "--project"])
        .assert()
        .success();
    let raw_after = std::fs::read_to_string(sb.project_config()).unwrap();
    assert!(
        !raw_after.contains("defaultModel"),
        "defaultModel key should be gone, got:\n{raw_after}"
    );
}

#[test]
fn show_reports_none_when_nothing_set() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .args(["models", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("No default model set") || s.contains("source: none"),
        "expected 'none' state in output:\n{s}"
    );
}
