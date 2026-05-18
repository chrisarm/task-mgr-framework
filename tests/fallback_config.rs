//! TDD scaffolding for US-005 — `FallbackRunnerConfig` schema + startup
//! binary check (FR-006).
//!
//! FEAT-004 will add:
//!
//!   * `pub fallback_runner: Option<FallbackRunnerConfig>` on
//!     [`ProjectConfig`] (`src/loop_engine/project_config.rs`).
//!   * `pub struct FallbackRunnerConfig { enabled: bool, provider: String,
//!     model: String, cli_binary: Option<String>, runtime_error_threshold:
//!     u32 }` with `#[serde(rename_all = "camelCase")]` and named
//!     `#[serde(default = "...")]` for every field that has a non-`bool`
//!     default (per learnings #2800 / #2366 / #2485 / #928).
//!   * `pub fn check_fallback_runner_binary(cfg:
//!     Option<&FallbackRunnerConfig>) -> Result<(), TaskMgrError>` — the
//!     startup binary-existence check called from `task-mgr loop start`
//!     before the first iteration. Ok when `None` or `enabled = false`;
//!     resolves `cli_binary` (or `"grok"` on PATH) and returns
//!     `TaskMgrError::ConfigError`/equivalent when missing.
//!
//! ## Why every test in this file is `#[ignore]`
//!
//! The type `FallbackRunnerConfig` does not exist yet. Importing it would
//! make this file fail to compile against `main`, breaking the
//! pre-FEAT-004 build. Tests therefore reference the type only inside
//! commented-out "future shape" blocks; their bodies call `panic!()` so a
//! stray `cargo test` (without `--ignored`) cannot silently report green.
//!
//! When FEAT-004 lands, the implementer MUST:
//!
//!   1. Add the imports listed at the top of each future-shape block.
//!   2. Replace each `panic!(…)` body with the commented assertions.
//!   3. Remove the `#[ignore]` attribute from every test below.
//!
//! The compile-only marker test [`test_file_compiles_marker`] runs
//! unconditionally so a future build break shows up as a failing test
//! rather than a silent rename / removal.

use std::fs;

use tempfile::TempDir;

use task_mgr::loop_engine::project_config::{ProjectConfig, read_project_config};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Writes `config.json` under a fresh tempdir and returns both. Mirrors the
/// helper shape used by the existing serde round-trip tests in
/// `project_config.rs` so FEAT-004's implementer can copy-paste from
/// there.
fn write_config(json: &str) -> (TempDir, ProjectConfig) {
    let dir = TempDir::new().expect("tempdir");
    fs::write(dir.path().join("config.json"), json).expect("write config.json");
    let cfg = read_project_config(dir.path());
    (dir, cfg)
}

/// Default Grok model id pinned in PRD §6 (Public Contracts). Tests rely
/// on the literal because `model.rs` does not yet expose a
/// `GROK_DEFAULT_MODEL` constant — FEAT-002 will add it.
const GROK_DEFAULT_MODEL: &str = "grok-4-fast";

/// Default provider name pinned in PRD §6.
const GROK_DEFAULT_PROVIDER: &str = "grok";

/// Default runtime-error escalation threshold (number of consecutive
/// `RuntimeError` rounds before the Grok fallback hook fires). PRD §3
/// US-005 default.
const RUNTIME_ERROR_THRESHOLD_DEFAULT: u32 = 2;

// ─────────────────────────────────────────────────────────────────────────────
// AC #1 — absent `fallbackRunner` key → field is None
// ─────────────────────────────────────────────────────────────────────────────

/// Today: `ProjectConfig` has no `fallback_runner` field — the entire JSON
/// is silently accepted and unknown keys are dropped (forward-compat
/// behavior the existing `test_read_config_with_unknown_fields` test
/// already locks in). After FEAT-004 this test must assert
/// `cfg.fallback_runner.is_none()`.
#[test]
#[ignore = "FEAT-004: ProjectConfig.fallback_runner field not yet defined"]
fn fallback_runner_absent_key_is_none() {
    // Future shape (FEAT-004):
    //   let (_dir, cfg) = write_config(r#"{"version": 1}"#);
    //   assert!(
    //       cfg.fallback_runner.is_none(),
    //       "absent `fallbackRunner` key must deserialize to None (not Some(default))",
    //   );

    let (_dir, _cfg) = write_config(r#"{"version": 1}"#);
    panic!(
        "FEAT-004 not yet wired — when implemented, ProjectConfig deserialized from \
         JSON without a `fallbackRunner` key MUST set `fallback_runner = None`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #2 — explicit `fallbackRunner: null` → field is None
// ─────────────────────────────────────────────────────────────────────────────

/// `Option<T>` with serde's default behavior treats `null` and absent
/// keys identically. This test pins the contract so a future
/// `#[serde(deserialize_with = ...)]` annotation that distinguishes the
/// two states (e.g., to surface a deprecation warning) breaks loudly.
#[test]
#[ignore = "FEAT-004: ProjectConfig.fallback_runner field not yet defined"]
fn fallback_runner_explicit_null_is_none() {
    // Future shape (FEAT-004):
    //   let (_dir, cfg) = write_config(r#"{"fallbackRunner": null}"#);
    //   assert!(
    //       cfg.fallback_runner.is_none(),
    //       "explicit `\"fallbackRunner\": null` MUST deserialize to None, identical to absent key",
    //   );

    let (_dir, _cfg) = write_config(r#"{"fallbackRunner": null}"#);
    panic!(
        "FEAT-004 not yet wired — when implemented, `\"fallbackRunner\": null` must deserialize \
         to `fallback_runner = None`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #3 — full object → every field parsed
// ─────────────────────────────────────────────────────────────────────────────

/// Pins the exact JSON shape (camelCase keys) AND the Rust-side field
/// names (snake_case). Both halves must move in lock-step or the
/// `#[serde(rename_all = "camelCase")]` attribute is misconfigured.
#[test]
#[ignore = "FEAT-004: FallbackRunnerConfig not yet defined"]
fn fallback_runner_full_object_round_trips() {
    // Future shape (FEAT-004):
    //   let (_dir, cfg) = write_config(r#"{
    //       "fallbackRunner": {
    //           "enabled": true,
    //           "provider": "grok",
    //           "model": "grok-4-fast",
    //           "cliBinary": "/opt/grok/bin/grok",
    //           "runtimeErrorThreshold": 5
    //       }
    //   }"#);
    //   let fr = cfg.fallback_runner.expect("fallback_runner deserialized");
    //   assert!(fr.enabled);
    //   assert_eq!(fr.provider, "grok");
    //   assert_eq!(fr.model, "grok-4-fast");
    //   assert_eq!(fr.cli_binary.as_deref(), Some("/opt/grok/bin/grok"));
    //   assert_eq!(fr.runtime_error_threshold, 5);

    let _ = GROK_DEFAULT_MODEL;
    let _ = GROK_DEFAULT_PROVIDER;
    panic!(
        "FEAT-004 not yet wired — when implemented, the full `fallbackRunner` JSON object \
         must deserialize into FallbackRunnerConfig with every field set verbatim"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #4 — partial object (missing runtimeErrorThreshold) → default 2
// ─────────────────────────────────────────────────────────────────────────────

/// Pins the named-default contract for `runtime_error_threshold`. The
/// default MUST come from a `#[serde(default = "default_fn")]` named
/// function (per learning #2800), not from `u32::default()` which would
/// silently give `0` and disable the escalation guard.
#[test]
#[ignore = "FEAT-004: FallbackRunnerConfig not yet defined"]
fn fallback_runner_missing_runtime_error_threshold_defaults_to_two() {
    // Future shape (FEAT-004):
    //   let (_dir, cfg) = write_config(r#"{
    //       "fallbackRunner": {
    //           "enabled": true,
    //           "provider": "grok",
    //           "model": "grok-4-fast"
    //       }
    //   }"#);
    //   let fr = cfg.fallback_runner.expect("fallback_runner deserialized");
    //   assert_eq!(
    //       fr.runtime_error_threshold, 2,
    //       "missing `runtimeErrorThreshold` must use the named-default fn (= 2), \
    //        NOT u32::default() (= 0) which would disable the escalation guard",
    //   );

    let _ = RUNTIME_ERROR_THRESHOLD_DEFAULT;
    panic!(
        "FEAT-004 not yet wired — when implemented, a `fallbackRunner` block missing \
         `runtimeErrorThreshold` must default to {RUNTIME_ERROR_THRESHOLD_DEFAULT} (named serde default)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #5 — partial object (missing model) → default 'grok-4-fast'
// ─────────────────────────────────────────────────────────────────────────────

/// Pins the named-default contract for `model`. The default model id
/// must match PRD §6 (`grok-4-fast`); a `String::default()` ("") would
/// pass schema validation but silently route subprocesses to an empty
/// model id, surfacing only as a Grok runtime error.
#[test]
#[ignore = "FEAT-004: FallbackRunnerConfig not yet defined"]
fn fallback_runner_missing_model_defaults_to_grok_4_fast() {
    // Future shape (FEAT-004):
    //   let (_dir, cfg) = write_config(r#"{
    //       "fallbackRunner": {
    //           "enabled": true
    //       }
    //   }"#);
    //   let fr = cfg.fallback_runner.expect("fallback_runner deserialized");
    //   assert_eq!(
    //       fr.model, "grok-4-fast",
    //       "missing `model` must use the named-default fn (= \"grok-4-fast\"), \
    //        NOT String::default() (= \"\")",
    //   );
    //   assert_eq!(fr.provider, "grok", "missing `provider` must default to \"grok\"");

    let _ = GROK_DEFAULT_MODEL;
    let _ = GROK_DEFAULT_PROVIDER;
    panic!(
        "FEAT-004 not yet wired — when implemented, a `fallbackRunner` block missing \
         `model` must default to \"{GROK_DEFAULT_MODEL}\" and missing `provider` to \
         \"{GROK_DEFAULT_PROVIDER}\""
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #6 — `enabled = false` → loader returns Some(cfg) (not None)
// ─────────────────────────────────────────────────────────────────────────────

/// Operator intent matters here: a present-but-disabled block is NOT
/// equivalent to `None`. Downstream code (the overflow rung-4 gate, the
/// runtime-error hook) uses `Option<&FallbackRunnerConfig>` to decide
/// whether to evaluate the gate at all; a `Some(cfg)` with `enabled =
/// false` keeps the operator's configured `model` / `cli_binary` /
/// `runtime_error_threshold` values intact for future re-enable, while
/// `None` discards them. This test pins the distinction.
#[test]
#[ignore = "FEAT-004: FallbackRunnerConfig not yet defined"]
fn fallback_runner_enabled_false_returns_some_not_none() {
    // Future shape (FEAT-004):
    //   let (_dir, cfg) = write_config(r#"{
    //       "fallbackRunner": {
    //           "enabled": false,
    //           "model": "grok-4-fast-tuned"
    //       }
    //   }"#);
    //   let fr = cfg.fallback_runner.expect(
    //       "present-but-disabled fallbackRunner MUST deserialize as Some(_), \
    //        NOT collapsed to None — operator's `model`/`cli_binary` values \
    //        must survive for future re-enable",
    //   );
    //   assert!(!fr.enabled);
    //   assert_eq!(fr.model, "grok-4-fast-tuned", "operator's model override preserved");

    panic!(
        "FEAT-004 not yet wired — when implemented, `fallbackRunner.enabled = false` must \
         yield `Some(cfg)` (with enabled=false), NOT collapse to None"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #7 — startup check FIRES when enabled=true AND binary missing
// ─────────────────────────────────────────────────────────────────────────────

/// FR-006 / PRD §6 Public Contracts: the startup binary-existence check
/// MUST refuse to launch the loop when `fallbackRunner.enabled = true`
/// and the resolved binary (`cli_binary` if set, else `"grok"` on PATH)
/// does not exist. The returned error message must NAME the missing
/// binary so operators can diagnose without re-reading config.
#[test]
#[ignore = "FEAT-004: check_fallback_runner_binary not yet defined"]
fn startup_check_fires_when_enabled_and_binary_missing() {
    // Future shape (FEAT-004):
    //   use task_mgr::loop_engine::project_config::{FallbackRunnerConfig, check_fallback_runner_binary};
    //
    //   let cfg = FallbackRunnerConfig {
    //       enabled: true,
    //       cli_binary: Some("/nonexistent/path/to/grok-binary-9b2f".to_string()),
    //       ..Default::default()
    //   };
    //   let result = check_fallback_runner_binary(Some(&cfg));
    //   let err = result.expect_err(
    //       "enabled=true + missing binary MUST return an Err; otherwise the loop \
    //        would launch and the first promotion attempt would hang the slot",
    //   );
    //   let msg = err.to_string();
    //   assert!(
    //       msg.contains("/nonexistent/path/to/grok-binary-9b2f"),
    //       "error message MUST name the missing binary path for diagnosability, got: {msg}",
    //   );

    panic!(
        "FEAT-004 not yet wired — when implemented, check_fallback_runner_binary(Some(cfg)) \
         with enabled=true + missing binary path MUST return Err with the missing path \
         in the message"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #8 — startup check is a NO-OP when disabled or absent
// ─────────────────────────────────────────────────────────────────────────────

/// Disabled / absent fallback config must NEVER invoke PATH probing —
/// operators who don't opt in MUST NOT see a Grok-related stat() syscall
/// or any error from a missing `grok` binary. This is the byte-identical
/// regression guard for the default-disabled config path.
#[test]
#[ignore = "FEAT-004: check_fallback_runner_binary not yet defined"]
fn startup_check_skipped_when_disabled_or_none() {
    // Future shape (FEAT-004):
    //   use task_mgr::loop_engine::project_config::{FallbackRunnerConfig, check_fallback_runner_binary};
    //
    //   // Case 1: None — operator hasn't configured fallback at all.
    //   check_fallback_runner_binary(None)
    //       .expect("None config MUST return Ok with no PATH probe");
    //
    //   // Case 2: Some but enabled=false. Use a deliberately-broken cli_binary
    //   // path: if the check were to fire, this would Err. Ok confirms the
    //   // function short-circuited on `enabled`.
    //   let disabled = FallbackRunnerConfig {
    //       enabled: false,
    //       cli_binary: Some("/definitely/not/a/real/grok/path".to_string()),
    //       ..Default::default()
    //   };
    //   check_fallback_runner_binary(Some(&disabled))
    //       .expect("enabled=false MUST short-circuit BEFORE probing cli_binary");

    panic!(
        "FEAT-004 not yet wired — when implemented, check_fallback_runner_binary(None) \
         and check_fallback_runner_binary(Some(cfg with enabled=false)) MUST both Ok \
         without touching cli_binary or PATH"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #9 — cli_binary precedence: explicit path wins; else PATH("grok")
// ─────────────────────────────────────────────────────────────────────────────

/// Precedence contract: `cli_binary = Some(p)` MUST be probed at `p`
/// verbatim (NOT searched on PATH — operators set this exactly to
/// pin a non-standard install path). `cli_binary = None` MUST fall back
/// to a PATH lookup for the bare name `"grok"`.
///
/// Implementation hint for FEAT-004: use the `which` crate IF it lands
/// as a dependency in this PR, otherwise `std::process::Command::new(p)
/// .arg("--version").output()`. The test does not pin the strategy —
/// only the observable precedence.
#[test]
#[ignore = "FEAT-004: check_fallback_runner_binary not yet defined"]
fn startup_check_cli_binary_precedence() {
    // Future shape (FEAT-004):
    //   use task_mgr::loop_engine::project_config::{FallbackRunnerConfig, check_fallback_runner_binary};
    //
    //   // Sub-case 1: cli_binary = Some("/usr/bin/true"). `/usr/bin/true` exists
    //   // on every Linux test host AND is not on PATH as `grok`, so this proves
    //   // the explicit path is honored verbatim (not re-resolved via PATH).
    //   if std::path::Path::new("/usr/bin/true").exists() {
    //       let cfg = FallbackRunnerConfig {
    //           enabled: true,
    //           cli_binary: Some("/usr/bin/true".to_string()),
    //           ..Default::default()
    //       };
    //       check_fallback_runner_binary(Some(&cfg))
    //           .expect("explicit cli_binary path that exists MUST pass the check");
    //   }
    //
    //   // Sub-case 2: cli_binary = None on a host where `grok` is NOT on PATH.
    //   // PATH-based check should Err with a message that names `grok` (so the
    //   // operator can `which grok` to confirm).
    //   if which::which("grok").is_err() {
    //       let cfg = FallbackRunnerConfig {
    //           enabled: true,
    //           cli_binary: None,
    //           ..Default::default()
    //       };
    //       let err = check_fallback_runner_binary(Some(&cfg))
    //           .expect_err("cli_binary=None + grok not on PATH MUST Err");
    //       assert!(
    //           err.to_string().to_lowercase().contains("grok"),
    //           "error MUST name the bare binary name `grok` for the PATH-lookup case",
    //       );
    //   }

    panic!(
        "FEAT-004 not yet wired — when implemented, check_fallback_runner_binary must honor \
         cli_binary=Some(p) verbatim (no PATH re-resolution) and fall back to PATH lookup \
         for `grok` when cli_binary=None"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #10 — test file compiles
// ─────────────────────────────────────────────────────────────────────────────

/// Compile-only marker. The file's successful build is the assertion;
/// this stub catches any future build break as a missing test rather
/// than a silent removal. Mirrors the marker pattern used by
/// `tests/overflow_fallback_rung.rs::test_file_compiles_marker` so the
/// review checklist can grep for `test_file_compiles_marker` and find
/// every TDD-scaffolding harness in one go.
#[test]
fn test_file_compiles_marker() {
    // Touch a symbol from each public type / constant referenced above
    // so the linker can't dead-code-eliminate the imports.
    let _: ProjectConfig = ProjectConfig::default();
    assert_eq!(GROK_DEFAULT_MODEL, "grok-4-fast");
    assert_eq!(GROK_DEFAULT_PROVIDER, "grok");
    assert_eq!(RUNTIME_ERROR_THRESHOLD_DEFAULT, 2);
}
