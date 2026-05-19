//! Tests for US-005 — `FallbackRunnerConfig` schema + startup binary check
//! (FR-006).

use std::fs;
use std::sync::Mutex;

use tempfile::TempDir;

use task_mgr::loop_engine::project_config::{
    FallbackRunnerConfig, ProjectConfig, check_fallback_runner_binary, read_project_config,
};

/// Serializes GROK_BINARY env-var mutations in this test binary.
/// check_fallback_runner_binary reads GROK_BINARY, so tests that set it
/// must hold this lock to avoid races with other tests.
static GROK_BINARY_MUTEX: Mutex<()> = Mutex::new(());

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
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // Clear GROK_BINARY so the env-var resolution chain does not intercept
    // the explicit cli_binary probe below (another test may have set it).
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::remove_var("GROK_BINARY") };

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
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // Clear GROK_BINARY so both sub-cases below see the env-var chain with
    // no override — the test is probing cli_binary and PATH precedence only.
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::remove_var("GROK_BINARY") };

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
// M3 — GROK_BINARY env var is honored by check_fallback_runner_binary
// ─────────────────────────────────────────────────────────────────────────────

/// M3: when `GROK_BINARY` points at a real binary (e.g. `/usr/bin/true`),
/// `check_fallback_runner_binary` should succeed even when `cli_binary` is
/// `None` and `grok` is NOT on PATH. This proves the startup check uses the
/// same resolution chain as `GrokRunner::spawn` (env var → cli_binary → PATH).
#[test]
fn startup_check_honors_grok_binary_env_var() {
    if !std::path::Path::new("/usr/bin/true").exists() {
        // Host doesn't have /usr/bin/true — skip rather than fail.
        return;
    }
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::set_var("GROK_BINARY", "/usr/bin/true") };

    let cfg = FallbackRunnerConfig {
        enabled: true,
        cli_binary: None, // env var must take precedence
        ..Default::default()
    };
    let result = check_fallback_runner_binary(Some(&cfg));

    unsafe { std::env::remove_var("GROK_BINARY") };

    result.expect(
        "GROK_BINARY=/usr/bin/true must pass the startup check even when \
         cli_binary is None — env var has the highest resolution priority",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// W1 — empty/whitespace GROK_BINARY falls through to cli_binary/PATH
// ─────────────────────────────────────────────────────────────────────────────

/// W1: an operator with `export GROK_BINARY=""` in their env should not get a
/// confusing startup failure when `cli_binary` is a real executable. The
/// runtime resolver (`runner::resolve_grok_binary`) already skips
/// empty/whitespace values; the startup check must agree.
#[test]
fn startup_check_skips_empty_grok_binary_env() {
    if !std::path::Path::new("/usr/bin/true").exists() {
        return;
    }
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::set_var("GROK_BINARY", "") };

    let cfg = FallbackRunnerConfig {
        enabled: true,
        cli_binary: Some("/usr/bin/true".to_string()),
        ..Default::default()
    };
    let result = check_fallback_runner_binary(Some(&cfg));

    unsafe { std::env::remove_var("GROK_BINARY") };

    result.expect(
        "GROK_BINARY=\"\" must fall through to cli_binary — the runtime \
         resolver does the same, and a divergence would surface as a \
         spurious startup failure on a misconfigured env",
    );
}

#[test]
fn startup_check_skips_whitespace_grok_binary_env() {
    if !std::path::Path::new("/usr/bin/true").exists() {
        return;
    }
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::set_var("GROK_BINARY", "   ") };

    let cfg = FallbackRunnerConfig {
        enabled: true,
        cli_binary: Some("/usr/bin/true".to_string()),
        ..Default::default()
    };
    let result = check_fallback_runner_binary(Some(&cfg));

    unsafe { std::env::remove_var("GROK_BINARY") };

    result.expect("whitespace-only GROK_BINARY must fall through to cli_binary");
}

// ─────────────────────────────────────────────────────────────────────────────
// W2 — non-executable file at the resolved path fails the check
// ─────────────────────────────────────────────────────────────────────────────

/// W2: on Unix, the startup check should reject a path that exists but is not
/// executable (e.g. a regular text file). Catches the misconfiguration up-front
/// instead of letting spawn fail with a less-helpful `std::io::Error` at
/// first promotion.
#[cfg(unix)]
#[test]
fn startup_check_rejects_non_executable_cli_binary() {
    let _guard = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env mutation is process-global; serialized via GROK_BINARY_MUTEX.
    unsafe { std::env::remove_var("GROK_BINARY") };

    let dir = TempDir::new().expect("tempdir");
    let non_exec = dir.path().join("not-executable.txt");
    fs::write(&non_exec, b"i am a regular file, not a binary").expect("write file");
    // Ensure no exec bit is set (default umask should already do this, but be
    // explicit so the test is deterministic across hosts).
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&non_exec).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&non_exec, perms).expect("set perms");

    let cfg = FallbackRunnerConfig {
        enabled: true,
        cli_binary: Some(non_exec.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let err = check_fallback_runner_binary(Some(&cfg))
        .expect_err("non-executable file at cli_binary path MUST fail the startup check");
    let msg = err.to_string();
    assert!(
        msg.contains(non_exec.to_str().unwrap()),
        "error message MUST name the offending path; got: {msg}",
    );
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
