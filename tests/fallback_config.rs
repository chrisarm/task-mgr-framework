//! Tests for US-005 — `FallbackRunnerConfig` schema + startup binary check
//! (FR-006).

use std::fs;

use tempfile::TempDir;

use task_mgr::loop_engine::project_config::{
    FallbackRunnerConfig, ProjectConfig, check_fallback_runner_binary, read_project_config,
};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn write_config(json: &str) -> (TempDir, ProjectConfig) {
    let dir = TempDir::new().expect("tempdir");
    fs::write(dir.path().join("config.json"), json).expect("write config.json");
    let cfg = read_project_config(dir.path());
    (dir, cfg)
}

const GROK_DEFAULT_MODEL: &str = "grok-4-fast";
const GROK_DEFAULT_PROVIDER: &str = "grok";
const RUNTIME_ERROR_THRESHOLD_DEFAULT: u32 = 2;

// ─────────────────────────────────────────────────────────────────────────────
// AC #1 — absent `fallbackRunner` key → field is None
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fallback_runner_absent_key_is_none() {
    let (_dir, cfg) = write_config(r#"{"version": 1}"#);
    assert!(
        cfg.fallback_runner.is_none(),
        "absent `fallbackRunner` key must deserialize to None (not Some(default))",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #2 — explicit `fallbackRunner: null` → field is None
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fallback_runner_explicit_null_is_none() {
    let (_dir, cfg) = write_config(r#"{"fallbackRunner": null}"#);
    assert!(
        cfg.fallback_runner.is_none(),
        "explicit `\"fallbackRunner\": null` MUST deserialize to None, identical to absent key",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #3 — full object → every field parsed
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fallback_runner_full_object_round_trips() {
    let (_dir, cfg) = write_config(
        r#"{
        "fallbackRunner": {
            "enabled": true,
            "provider": "grok",
            "model": "grok-4-fast",
            "cliBinary": "/opt/grok/bin/grok",
            "runtimeErrorThreshold": 5
        }
    }"#,
    );
    let fr = cfg.fallback_runner.expect("fallback_runner deserialized");
    assert!(fr.enabled);
    assert_eq!(fr.provider, "grok");
    assert_eq!(fr.model, "grok-4-fast");
    assert_eq!(fr.cli_binary.as_deref(), Some("/opt/grok/bin/grok"));
    assert_eq!(fr.runtime_error_threshold, 5);
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #4 — partial object (missing runtimeErrorThreshold) → default 2
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fallback_runner_missing_runtime_error_threshold_defaults_to_two() {
    let (_dir, cfg) = write_config(
        r#"{
        "fallbackRunner": {
            "enabled": true,
            "provider": "grok",
            "model": "grok-4-fast"
        }
    }"#,
    );
    let fr = cfg.fallback_runner.expect("fallback_runner deserialized");
    assert_eq!(
        fr.runtime_error_threshold, RUNTIME_ERROR_THRESHOLD_DEFAULT,
        "missing `runtimeErrorThreshold` must use the named-default fn (= 2), \
         NOT u32::default() (= 0) which would disable the escalation guard",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #5 — partial object (missing model) → default 'grok-4-fast'
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fallback_runner_missing_model_defaults_to_grok_4_fast() {
    let (_dir, cfg) = write_config(
        r#"{
        "fallbackRunner": {
            "enabled": true
        }
    }"#,
    );
    let fr = cfg.fallback_runner.expect("fallback_runner deserialized");
    assert_eq!(
        fr.model, GROK_DEFAULT_MODEL,
        "missing `model` must use the named-default fn (= \"grok-4-fast\"), \
         NOT String::default() (= \"\")",
    );
    assert_eq!(
        fr.provider, GROK_DEFAULT_PROVIDER,
        "missing `provider` must default to \"grok\"",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #6 — `enabled = false` → loader returns Some(cfg) (not None)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fallback_runner_enabled_false_returns_some_not_none() {
    let (_dir, cfg) = write_config(
        r#"{
        "fallbackRunner": {
            "enabled": false,
            "model": "grok-4-fast-tuned"
        }
    }"#,
    );
    let fr = cfg.fallback_runner.expect(
        "present-but-disabled fallbackRunner MUST deserialize as Some(_), \
         NOT collapsed to None — operator's `model`/`cli_binary` values \
         must survive for future re-enable",
    );
    assert!(!fr.enabled);
    assert_eq!(
        fr.model, "grok-4-fast-tuned",
        "operator's model override preserved"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #7 — startup check FIRES when enabled=true AND binary missing
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn startup_check_fires_when_enabled_and_binary_missing() {
    let cfg = FallbackRunnerConfig {
        enabled: true,
        cli_binary: Some("/nonexistent/path/to/grok-binary-9b2f".to_string()),
        ..Default::default()
    };
    let result = check_fallback_runner_binary(Some(&cfg));
    let err = result.expect_err(
        "enabled=true + missing binary MUST return an Err; otherwise the loop \
         would launch and the first promotion attempt would hang the slot",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("/nonexistent/path/to/grok-binary-9b2f"),
        "error message MUST name the missing binary path for diagnosability, got: {msg}",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #8 — startup check is a NO-OP when disabled or absent
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn startup_check_skipped_when_disabled_or_none() {
    // Case 1: None — operator hasn't configured fallback at all.
    check_fallback_runner_binary(None).expect("None config MUST return Ok with no PATH probe");

    // Case 2: Some but enabled=false. Use a deliberately-broken cli_binary
    // path: if the check were to fire, this would Err. Ok confirms the
    // function short-circuited on `enabled`.
    let disabled = FallbackRunnerConfig {
        enabled: false,
        cli_binary: Some("/definitely/not/a/real/grok/path".to_string()),
        ..Default::default()
    };
    check_fallback_runner_binary(Some(&disabled))
        .expect("enabled=false MUST short-circuit BEFORE probing cli_binary");
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #9 — cli_binary precedence: explicit path wins; else PATH("grok")
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn startup_check_cli_binary_precedence() {
    // Sub-case 1: cli_binary = Some("/usr/bin/true"). `/usr/bin/true` exists
    // on every Linux test host AND is not on PATH as `grok`, so this proves
    // the explicit path is honored verbatim (not re-resolved via PATH).
    if std::path::Path::new("/usr/bin/true").exists() {
        let cfg = FallbackRunnerConfig {
            enabled: true,
            cli_binary: Some("/usr/bin/true".to_string()),
            ..Default::default()
        };
        check_fallback_runner_binary(Some(&cfg))
            .expect("explicit cli_binary path that exists MUST pass the check");
    }

    // Sub-case 2: cli_binary = None on a host where `grok` is NOT on PATH.
    // PATH-based check should Err with a message that names `grok` (so the
    // operator can `which grok` to confirm).
    let grok_on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join("grok").exists()))
        .unwrap_or(false);
    if !grok_on_path {
        let cfg = FallbackRunnerConfig {
            enabled: true,
            cli_binary: None,
            ..Default::default()
        };
        let err = check_fallback_runner_binary(Some(&cfg))
            .expect_err("cli_binary=None + grok not on PATH MUST Err");
        assert!(
            err.to_string().to_lowercase().contains("grok"),
            "error MUST name the bare binary name `grok` for the PATH-lookup case",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #10 — test file compiles
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_file_compiles_marker() {
    let _: ProjectConfig = ProjectConfig::default();
    assert_eq!(GROK_DEFAULT_MODEL, "grok-4-fast");
    assert_eq!(GROK_DEFAULT_PROVIDER, "grok");
    assert_eq!(RUNTIME_ERROR_THRESHOLD_DEFAULT, 2);
}
