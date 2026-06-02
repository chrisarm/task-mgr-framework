//! codex-runner — Preserve provider identity through recovery + overflow +
//! non-counting Codex auth crash.
//!
//! These tests pin the acceptance criteria that the recovery / escalation /
//! overflow path NEVER promotes or escalates a task that actually ran on Codex
//! to Grok — the routing-to-Codex contract is hint-only and a re-derive from
//! a `gpt-*` model id would silently misroute to Claude.
//!
//! Ported from V1 and adapted for the merged branch API:
//!  - `escalate_task_model_if_needed_for_runner` (explicit runner variant)
//!  - `handle_task_failure_with_runner` (explicit runner variant)
//!  - `crash_counts_as_task_failure` replaces V1's `is_non_counting_auth_failure`
//!  - FEAT-005 fallback test added (not in V1): Codex + runtimeErrorFallback:true
//!    → exactly one Claude promotion in `runner_overrides`.

use std::collections::HashMap;

use rusqlite::Connection;
use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::config::CrashType;
use task_mgr::loop_engine::engine::{
    IterationContext, escalate_task_model_if_needed_for_runner, handle_task_failure_with_runner,
};
use task_mgr::loop_engine::model::{
    HAIKU_MODEL, ModelResolutionContext, OPUS_MODEL, Provider, SONNET_MODEL,
    resolve_task_execution_target,
};
use task_mgr::loop_engine::project_config::{
    FallbackRunnerConfig, PrimaryRunnerConfig, RunnerSpec,
};
use task_mgr::loop_engine::runner::{RunnerKind, codex_conversation_indicates_auth_failure};

fn setup_db() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut conn = open_connection(tmp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (tmp, conn)
}

fn insert_task(conn: &Connection, id: &str, model: Option<&str>) {
    let model_v: Option<String> = model.map(|s| s.to_string());
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, consecutive_failures, max_retries, priority) \
         VALUES (?, ?, 'in_progress', ?, ?, 3, 10)",
        rusqlite::params![id, format!("Test task {id}"), model_v, 0_i32],
    )
    .expect("insert task");
}

fn read_model(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT model FROM tasks WHERE id = ?", [id], |r| {
        r.get::<_, Option<String>>(0)
    })
    .expect("read model")
}

fn read_consecutive_failures(conn: &Connection, id: &str) -> i32 {
    conn.query_row(
        "SELECT consecutive_failures FROM tasks WHERE id = ?",
        [id],
        |r| r.get(0),
    )
    .expect("read consecutive_failures")
}

fn read_status(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |r| r.get(0))
        .expect("read status")
}

// ── AC: Codex RuntimeError with NULL DB model writes NO model and triggers NO
// Grok promotion ──────────────────────────────────────────────────────────────

/// A Codex task with a NULL `tasks.model` whose iteration just crashed with
/// `RuntimeError` MUST NOT route through the Claude escalation / Grok promotion
/// branch. `escalate_task_model_if_needed_for_runner` with `executed_runner =
/// Codex` must early-return without writing ANY model to `tasks.model`. The
/// Claude→Grok fallback config is enabled here to prove the Codex early-return
/// wins over the Grok-promotion branch.
#[test]
fn codex_runtime_error_null_db_model_writes_no_model_and_no_promotion() {
    let (_tmp, conn) = setup_db();
    insert_task(&conn, "CODEX-NULL-001", None);

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        provider: "grok".to_string(),
        model: "grok-build".to_string(),
        runtime_error_threshold: 2,
        ..Default::default()
    };

    // `new_count = 2` matches the escalation threshold AND the Grok-promotion
    // `runtime_error_threshold`, so both branches would fire were it not for
    // the Codex early-return in `escalate_task_model_if_needed_inner`.
    let outcome = escalate_task_model_if_needed_for_runner(
        &conn,
        "CODEX-NULL-001",
        2,
        RunnerKind::Codex,
        &mut ctx,
        Some(&cfg),
        None,
        None,
        None,
    )
    .expect("escalate_task_model_if_needed_for_runner");

    assert_eq!(
        outcome, None,
        "Codex executed_runner MUST short-circuit escalation (no escalated model returned)"
    );
    assert_eq!(
        read_model(&conn, "CODEX-NULL-001"),
        None,
        "Codex escalation must NOT write a Claude model to tasks.model when DB column was NULL"
    );
    assert!(
        !ctx.runner_overrides.contains_key("CODEX-NULL-001"),
        "Codex executed_runner MUST NOT trigger Grok promotion (no runner_overrides write)"
    );
    assert!(
        !ctx.model_overrides.contains_key("CODEX-NULL-001"),
        "Codex executed_runner MUST NOT trigger model_overrides write"
    );
    assert!(
        !ctx.overflow_original_task_model
            .contains_key("CODEX-NULL-001"),
        "no promotion → no overflow_original_task_model snapshot"
    );
}

/// A Codex task with a populated `gpt-*` `tasks.model` whose iteration just
/// crashed MUST also short-circuit. Without the explicit-runner path, the inner
/// function would map `gpt-4o` → `Provider::Claude` → wrong escalation branch.
#[test]
fn codex_runtime_error_populated_gpt_model_writes_no_model_and_no_promotion() {
    let (_tmp, conn) = setup_db();
    insert_task(&conn, "CODEX-GPT-001", Some("gpt-4o"));

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        provider: "grok".to_string(),
        model: "grok-build".to_string(),
        runtime_error_threshold: 2,
        ..Default::default()
    };

    let outcome = escalate_task_model_if_needed_for_runner(
        &conn,
        "CODEX-GPT-001",
        2,
        RunnerKind::Codex,
        &mut ctx,
        Some(&cfg),
        None,
        None,
        None,
    )
    .expect("escalate_task_model_if_needed_for_runner");

    assert_eq!(
        outcome, None,
        "Codex executed_runner MUST short-circuit on populated gpt-* model too"
    );
    assert_eq!(
        read_model(&conn, "CODEX-GPT-001").as_deref(),
        Some("gpt-4o"),
        "Codex tasks.model MUST be byte-identical after a Codex RuntimeError pass"
    );
    assert!(
        !ctx.runner_overrides.contains_key("CODEX-GPT-001"),
        "no Grok promotion for a Codex-executed task"
    );
}

// ── AC: Sequential: Codex crash via handle_task_failure increments
// consecutive_failures only as intended ───────────────────────────────────────

/// `handle_task_failure_with_runner` with `executed_runner = Some(Codex)` MUST
/// still increment `consecutive_failures` (auto-block accounting is the same for
/// every runner) but MUST NOT escalate the model or promote to Grok. After the
/// max-retries threshold, the task MUST auto-block via the regular path.
#[test]
fn handle_task_failure_codex_increments_counter_but_does_not_promote() {
    let (_tmp, mut conn) = setup_db();
    insert_task(&conn, "CODEX-RT-001", Some("gpt-4o"));

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        provider: "grok".to_string(),
        model: "grok-build".to_string(),
        runtime_error_threshold: 2,
        ..Default::default()
    };

    // Failure 1: count → 1. Below escalate threshold, below auto-block.
    handle_task_failure_with_runner(
        &mut conn,
        "CODEX-RT-001",
        1,
        &mut ctx,
        Some(RunnerKind::Codex),
        Some(&cfg),
        None,
        None,
        None,
    )
    .expect("handle_task_failure_with_runner 1");
    assert_eq!(read_consecutive_failures(&conn, "CODEX-RT-001"), 1);
    assert_eq!(
        read_model(&conn, "CODEX-RT-001").as_deref(),
        Some("gpt-4o"),
        "model unchanged on first failure"
    );

    // Failure 2: count → 2. Escalation threshold reached, but Codex
    // short-circuits → no model write, no Grok promotion.
    handle_task_failure_with_runner(
        &mut conn,
        "CODEX-RT-001",
        2,
        &mut ctx,
        Some(RunnerKind::Codex),
        Some(&cfg),
        None,
        None,
        None,
    )
    .expect("handle_task_failure_with_runner 2");
    assert_eq!(read_consecutive_failures(&conn, "CODEX-RT-001"), 2);
    assert_eq!(
        read_model(&conn, "CODEX-RT-001").as_deref(),
        Some("gpt-4o"),
        "Codex tasks.model MUST be unchanged after escalation-threshold failure"
    );
    assert!(
        !ctx.runner_overrides.contains_key("CODEX-RT-001"),
        "Codex MUST NOT be promoted to Grok"
    );

    // Failure 3: count → 3 >= max_retries(3). Auto-block fires.
    handle_task_failure_with_runner(
        &mut conn,
        "CODEX-RT-001",
        3,
        &mut ctx,
        Some(RunnerKind::Codex),
        Some(&cfg),
        None,
        None,
        None,
    )
    .expect("handle_task_failure_with_runner 3");
    assert_eq!(read_consecutive_failures(&conn, "CODEX-RT-001"), 3);
    assert_eq!(
        read_status(&conn, "CODEX-RT-001"),
        "blocked",
        "Codex task auto-blocks at max_retries — the standard accounting still fires"
    );
    assert_eq!(
        read_model(&conn, "CODEX-RT-001").as_deref(),
        Some("gpt-4o"),
        "Codex tasks.model MUST remain at gpt-4o through the entire failure cycle"
    );
    assert!(
        !ctx.runner_overrides.contains_key("CODEX-RT-001"),
        "no Grok promotion ever fired"
    );
}

// ── AC: is_non_counting_auth_failure — GrokAuthFailure and CodexAuthFailure
// are non-counting; RuntimeError, OomOrKilled, PromptTooLong are counting ────

/// The shared `crash_counts_as_task_failure` helper MUST classify
/// `CodexAuthFailure` and `GrokAuthFailure` as non-counting, and MUST NOT
/// misclassify common crash outcomes.
#[test]
fn auth_failure_variants_are_non_counting() {
    use task_mgr::loop_engine::config::crash_counts_as_task_failure;

    // Auth failures MUST be non-counting (cascade prevention).
    assert!(
        !crash_counts_as_task_failure(&CrashType::GrokAuthFailure),
        "GrokAuthFailure must be non-counting"
    );
    assert!(
        !crash_counts_as_task_failure(&CrashType::CodexAuthFailure),
        "CodexAuthFailure must be non-counting"
    );

    // Negative controls — these MUST stay counting so they reach the
    // failure-tracking / escalation path.
    assert!(crash_counts_as_task_failure(&CrashType::RuntimeError));
    assert!(crash_counts_as_task_failure(&CrashType::OomOrKilled));
    assert!(crash_counts_as_task_failure(&CrashType::PromptTooLong));
}

// ── AC: Negative-control: agent_message containing "HTTP 401" is NOT
// classified as CodexAuthFailure ─────────────────────────────────────────────

/// The `codex_conversation_indicates_auth_failure` classifier MUST only
/// match STRUCTURED `[Error: ...]` lines (emitted from `type:"error"` /
/// `type:"turn.failed"` stream events) — never plain agent text quoting an
/// HTTP 401 status. A model that writes `"I got an HTTP 401 from the API
/// when I tried to fetch ..."` in its `agent_message` MUST NOT auto-block
/// the runner; that text belongs in `assistant_buf` (the output channel),
/// not in the transcript with the `[Error: ` prefix.
#[test]
fn codex_conversation_indicates_auth_failure_is_structured_only() {
    // Negative-control: agent text mentioning auth markers without the
    // structured `[Error: ` prefix.
    let agent_text_mentioning_401 = "I tried to call the API and received an HTTP 401 response, \
        which usually means the bearer token is missing or invalid. Let me continue with the task.\n";
    assert!(
        !codex_conversation_indicates_auth_failure(agent_text_mentioning_401),
        "agent_message mentioning HTTP 401 in prose MUST NOT trip the auth classifier"
    );

    let agent_text_with_brackets_but_not_error = "Tool output: [Info: 401 records updated]\n\
        [Note: bearer tokens are rotated nightly]\n";
    assert!(
        !codex_conversation_indicates_auth_failure(agent_text_with_brackets_but_not_error),
        "bracketed prose that is not an `[Error: ` line MUST NOT trip the auth classifier"
    );

    // Positive control: a structured `[Error: ...]` line with an auth marker
    // MUST trip the classifier.
    let structured_401_error =
        "Some assistant text first.\n[Error: HTTP 401 unauthorized — token expired]\nMore text.\n";
    assert!(
        codex_conversation_indicates_auth_failure(structured_401_error),
        "structured [Error: ...] with 401 marker MUST classify as auth failure"
    );

    let structured_unauthorized = "[Error: unauthorized — please re-authenticate]\n";
    assert!(codex_conversation_indicates_auth_failure(
        structured_unauthorized
    ));

    let structured_missing_bearer = "[Error: missing bearer token in request]\n";
    assert!(codex_conversation_indicates_auth_failure(
        structured_missing_bearer
    ));

    let structured_invalid_api_key = "[Error: invalid API key]\n";
    assert!(codex_conversation_indicates_auth_failure(
        structured_invalid_api_key
    ));

    // Negative control: structured error line with NO auth marker — e.g., a
    // model timeout or a rate-limit. MUST NOT trip the classifier.
    let structured_non_auth_error = "[Error: rate limit exceeded, retry in 60s]\n";
    assert!(
        !codex_conversation_indicates_auth_failure(structured_non_auth_error),
        "structured errors without auth markers MUST NOT trip the auth classifier"
    );

    // Empty transcript — nothing to match.
    assert!(!codex_conversation_indicates_auth_failure(""));
}

// ── AC: Source-grep: escalate_task_model_if_needed_inner does NOT call
// resolve_effective_runner ────────────────────────────────────────────────────

/// The recovery hook must NEVER re-derive the runner from the model string —
/// the executed runner is threaded in from the call site so a Codex task's
/// `gpt-*` model cannot silently re-derive to Claude and trigger Opus escalation.
#[test]
fn escalate_task_model_if_needed_inner_does_not_call_resolve_effective_runner() {
    let source = std::fs::read_to_string("src/loop_engine/recovery.rs")
        .expect("could not read src/loop_engine/recovery.rs from tests/ cwd");

    let start = source
        .find("pub(crate) fn escalate_task_model_if_needed_inner(")
        .expect(
            "expected `pub(crate) fn escalate_task_model_if_needed_inner(` to be defined in recovery.rs",
        );

    // Find the opening brace of the function body and walk braces to find
    // the matching close. This isolates the function body proper — it does
    // NOT include the docstring of the next top-level fn.
    let body_open_rel = source[start..]
        .find('{')
        .expect("expected `{` opening the function body");
    let body_open_abs = start + body_open_rel;
    let after_open = &source[body_open_abs + 1..];
    let mut depth = 1_i32;
    let mut body_end_rel = 0;
    for (i, ch) in after_open.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    body_end_rel = i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        body_end_rel > 0,
        "expected to find matching closing brace for escalate_task_model_if_needed_inner"
    );
    let body = &after_open[..body_end_rel];

    assert!(
        !body.contains("resolve_effective_runner"),
        "escalate_task_model_if_needed_inner MUST NOT call `resolve_effective_runner` — \
         the executed runner is threaded from the call site (IterationResult.effective_runner) \
         so a Codex task's `gpt-*` model cannot silently re-derive to Claude."
    );

    // Affirmative: the function must actually use the threaded `executed_runner`
    // parameter to dispatch its match.
    assert!(
        body.contains("executed_runner"),
        "escalate_task_model_if_needed_inner MUST reference its `executed_runner` parameter \
         in the body — otherwise the threading is a no-op."
    );
}

// ── AC: Source-grep: IterationResult.effective_runner is populated at
// every post-spawn build site ─────────────────────────────────────────────────

/// The CONTRACT: every IterationResult built AT a runner spawn site MUST
/// carry the spawn-time `effective_runner` on the struct (not a sentinel
/// default). This grep test sweeps the two spawn paths (sequential
/// `iteration.rs::run_iteration` and wave `slot.rs::run_slot_iteration`)
/// and asserts each IterationResult literal contains an `effective_runner:`
/// field.
#[test]
fn iteration_result_effective_runner_populated_at_spawn_sites() {
    for path in ["src/loop_engine/iteration.rs", "src/loop_engine/slot.rs"] {
        let source =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
        let mut byte_idx = 0;
        let mut sites = 0;
        while let Some(rel) = source[byte_idx..].find("IterationResult {") {
            let lit_start = byte_idx + rel;
            byte_idx = lit_start + "IterationResult {".len();
            let after = &source[byte_idx..];
            let mut depth = 1_i32;
            let mut end = 0;
            for (i, ch) in after.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body = &after[..end];
            let has_explicit = body.contains("effective_runner:");
            let has_shorthand =
                body.contains("effective_runner,") || body.trim_end().ends_with("effective_runner");
            assert!(
                has_explicit || has_shorthand,
                "every `IterationResult {{ ... }}` literal in {path} MUST set \
                 `effective_runner` (explicit `effective_runner:` or shorthand \
                 `effective_runner,`) — found one at byte {lit_start} that does NOT. \
                 FEAT-003 CONTRACT: the spawn-time runner identity is threaded onto \
                 IterationResult so recovery can never re-derive it from the model string."
            );
            sites += 1;
        }
        assert!(
            sites > 0,
            "expected at least one IterationResult literal in {path}"
        );
    }
}

// ── NEW: FEAT-005 fallback test — Codex runtime failure + runtimeErrorFallback:true
// → runner_overrides carries RunnerKind::Claude exactly once ──────────────────

/// A Codex task whose `primaryRunner` spec has `runtimeErrorFallback: true` MUST
/// be promoted to `RunnerKind::Claude` on the first runtime failure at threshold.
///
/// Known-bad negative: a test that only checks `runner_overrides` is NOT Codex
/// (i.e. `assert_ne!(kind, RunnerKind::Codex)`) would pass a buggy
/// promote-to-Codex implementation. We assert the specific value is
/// `RunnerKind::Claude`.
///
/// Idempotency: a second failure call with Codex runner MUST NOT re-promote
/// (the `runner_overrides` entry blocks the fallback branch).
#[test]
fn codex_runtime_failure_with_fallback_to_claude_promotes_to_claude_once() {
    let (_tmp, conn) = setup_db();
    // Codex tasks typically have NULL model (model is owned by the route spec).
    conn.execute(
        "INSERT INTO tasks (id, title, status, consecutive_failures, max_retries, priority) \
         VALUES ('SPIKE-FALLBACK-001', 'Codex fallback test', 'in_progress', 0, 5, 10)",
        [],
    )
    .expect("insert task");

    let mut ctx = IterationContext::new(8);
    let mut by_id_prefix = HashMap::new();
    by_id_prefix.insert(
        "SPIKE-".to_string(),
        RunnerSpec {
            provider: "codex".to_string(),
            model: String::new(),
            runtime_error_fallback: true,
        },
    );
    let primary_cfg = PrimaryRunnerConfig {
        by_id_prefix,
        ..Default::default()
    };

    // First failure at threshold → should promote to Claude.
    let result = escalate_task_model_if_needed_for_runner(
        &conn,
        "SPIKE-FALLBACK-001",
        2,
        RunnerKind::Codex,
        &mut ctx,
        None,
        Some(&primary_cfg),
        None,
        None,
    )
    .expect("escalate first failure");

    assert!(
        result.is_some(),
        "runtimeErrorFallback:true at threshold MUST return Some(target_model)"
    );
    assert_eq!(
        ctx.runner_overrides.get("SPIKE-FALLBACK-001").copied(),
        Some(RunnerKind::Claude),
        "Codex→Claude promotion MUST insert RunnerKind::Claude into runner_overrides, \
         not RunnerKind::Codex or RunnerKind::Grok"
    );
    assert!(
        ctx.model_overrides.contains_key("SPIKE-FALLBACK-001"),
        "model_overrides MUST carry the promoted Claude model"
    );
    let promoted_model = ctx
        .model_overrides
        .get("SPIKE-FALLBACK-001")
        .expect("model_overrides entry");
    assert!(
        !promoted_model.is_empty(),
        "promoted Claude model MUST be non-empty"
    );

    // Second failure with Codex runner MUST NOT re-promote (idempotency).
    let result2 = escalate_task_model_if_needed_for_runner(
        &conn,
        "SPIKE-FALLBACK-001",
        3,
        RunnerKind::Codex,
        &mut ctx,
        None,
        Some(&primary_cfg),
        None,
        None,
    )
    .expect("escalate second failure");

    assert_eq!(
        result2, None,
        "already-promoted task MUST NOT re-promote — idempotency guard must prevent \
         the second Codex failure from overwriting the Claude override"
    );
    assert_eq!(
        ctx.runner_overrides.get("SPIKE-FALLBACK-001").copied(),
        Some(RunnerKind::Claude),
        "runner_overrides MUST remain Claude after the second Codex failure"
    );
}

#[test]
fn codex_baseline_tier_route_with_fallback_to_claude_promotes() {
    let (_tmp, conn) = setup_db();
    conn.execute(
        "INSERT INTO tasks \
         (id, title, status, difficulty, consecutive_failures, max_retries, priority) \
         VALUES ('FEAT-TIER-001', 'Codex tier fallback test', 'in_progress', 'high', 0, 5, 10)",
        [],
    )
    .expect("insert task");

    let mut tiers = HashMap::new();
    tiers.insert(
        "high".to_string(),
        RunnerSpec {
            provider: "codex".to_string(),
            model: String::new(),
            runtime_error_fallback: true,
        },
    );
    let mut baseline_tier_routes = HashMap::new();
    baseline_tier_routes.insert("FEAT".to_string(), tiers);
    let primary_cfg = PrimaryRunnerConfig {
        baseline_tier_routes,
        ..Default::default()
    };
    let mut ctx = IterationContext::new(8);

    let result = escalate_task_model_if_needed_for_runner(
        &conn,
        "FEAT-TIER-001",
        2,
        RunnerKind::Codex,
        &mut ctx,
        None,
        Some(&primary_cfg),
        None,
        None,
    )
    .expect("baseline-tier codex fallback");

    assert_eq!(result.as_deref(), Some(OPUS_MODEL));
    assert_eq!(
        ctx.runner_overrides.get("FEAT-TIER-001").copied(),
        Some(RunnerKind::Claude),
        "baselineTierRoutes Codex route with runtimeErrorFallback must promote to Claude"
    );
    assert_eq!(
        ctx.model_overrides.get("FEAT-TIER-001").map(String::as_str),
        Some(OPUS_MODEL),
        "high-difficulty FEAT baseline promotes to Opus on Claude fallback"
    );
}

// ── TEST-INIT-001: recovery↔primary baseline-tier parity ──────────────────────
//
// These pin the contract that a *recovering* Codex task derives the SAME
// baseline tier — and therefore matches the SAME `baselineTierRoutes` route — as
// the spawn-time primary resolution (`resolve_task_execution_target`) does for
// identical inputs.
//
// Confirmed divergence (recovery.rs `maybe_codex_fallback_to_claude`): recovery
// substitutes `primary.claude_fallback_model` for `project_default` and omits
// `user_default` entirely, so for a NON-high task whose baseline tier comes from
// the real project/user defaults, recovery matches a DIFFERENT route than spawn
// did. FIX-001 closes this by threading the engine-cached project/user defaults
// (engine.rs:159-161) down the failure-handler chain and deriving the baseline
// via `model::compute_baseline_model` — the SSoT the primary site already uses.

/// `primaryRunner` config whose `FEAT` prefix routes BOTH the `low` and `high`
/// baseline tiers to Codex with `runtimeErrorFallback: true`. Mirrors the
/// `make_baseline_tier_cfg` fixture in `tests/primary_runner_routing.rs` so the
/// recovery side is exercised against the same route shape the primary-site
/// tests assert.
fn feat_codex_tier_cfg() -> PrimaryRunnerConfig {
    let codex_fallback = || RunnerSpec {
        provider: "codex".to_string(),
        model: String::new(),
        runtime_error_fallback: true,
    };
    let mut tiers = HashMap::new();
    tiers.insert("low".to_string(), codex_fallback());
    tiers.insert("high".to_string(), codex_fallback());
    let mut baseline_tier_routes = HashMap::new();
    baseline_tier_routes.insert("FEAT".to_string(), tiers);
    PrimaryRunnerConfig {
        baseline_tier_routes,
        ..Default::default()
    }
}

/// Insert a Codex-routed FEAT task (NULL model — the model is owned by the route
/// spec) with an explicit difficulty so the recovery baseline derivation has a
/// production-shaped DB row to read.
fn insert_feat_task(conn: &Connection, id: &str, difficulty: &str) {
    conn.execute(
        "INSERT INTO tasks \
         (id, title, status, difficulty, consecutive_failures, max_retries, priority) \
         VALUES (?, ?, 'in_progress', ?, 0, 5, 10)",
        rusqlite::params![id, format!("Test task {id}"), difficulty],
    )
    .expect("insert feat task");
}

/// AC#2 — positive-control parity anchor. A difficulty=`high` FEAT task with a
/// `high`-tier Codex route resolves, at the PRIMARY site, to `model = None` +
/// `provider_hint = Codex` (the route's empty model means "owned by the route";
/// the provider hint carries the Codex intent).
///
/// The high-difficulty path short-circuits to OPUS at BOTH the primary site and
/// in recovery (`compute_baseline_model(Some("high"), ..) == OPUS_MODEL`
/// regardless of project/user defaults), so the two sites already AGREE today —
/// there is NO divergence to fix on the high path. The recovery-side counterpart
/// is pinned by `codex_baseline_tier_route_with_fallback_to_claude_promotes`
/// above (high FEAT Codex task → promotes to OPUS via the same `high` route).
///
/// Mirrors `feat_baseline_tier_routes_standard_to_grok_and_high_to_codex` in
/// `tests/primary_runner_routing.rs`.
#[test]
fn primary_high_difficulty_routes_to_codex_high_tier() {
    let cfg = feat_codex_tier_cfg();
    let target = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some("8d71d1f7-FEAT-001"),
        difficulty: Some("high"),
        // Defaults are irrelevant on the high path — high short-circuits to OPUS.
        project_default: Some(HAIKU_MODEL),
        user_default: Some(HAIKU_MODEL),
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(
        target.model, None,
        "a high-tier Codex route carries an empty model (owned by the route) → model None"
    );
    assert_eq!(
        target.provider_hint,
        Some(Provider::Codex),
        "difficulty=high → OPUS baseline → `high` tier → Codex route → provider_hint=Codex"
    );
}

/// AC#1 + AC#3 — known-bad discriminator. A NON-high Codex task whose baseline
/// tier is determined by the REAL `project_default` (sub-case A) or `user_default`
/// (sub-case B) must, in recovery, match the SAME `baselineTierRoutes` route the
/// primary site routes it through — i.e. the Codex `runtimeErrorFallback` route
/// must fire and promote the task to Claude.
///
/// AGAINST CURRENT recovery code this FAILS: `maybe_codex_fallback_to_claude`
/// substitutes `primary.claude_fallback_model` (unset → None) for project_default
/// and omits user_default, so the baseline derives to `None` → tier `Default` →
/// NO `baselineTierRoutes` route matches → no promotion fires. AFTER FIX-001
/// threads the real project/user defaults through and derives the baseline via
/// `compute_baseline_model`, the tier resolves to Haiku → the `low` route matches
/// → the task is promoted, and the assertions pass.
///
/// NOTE ON INPUTS: AC#1 lists `prd_default = SONNET`. With prd_default present
/// the PRD rung (rung 4) dominates `compute_baseline_model` on BOTH the buggy and
/// fixed paths, so the divergence would be masked and the test would not
/// discriminate. We therefore drop prd_default (no `prd_metadata` row) so the
/// baseline is driven by project/user — exactly the inputs the bug mishandles.
/// The progress log records this surfaced discrepancy.
#[test]
fn recovery_baseline_tier_parity_non_high_codex() {
    // -- Sub-case A: baseline driven by project_default --------------------------
    {
        let (_tmp, conn) = setup_db();
        let task_id = "8d71d1f7-FEAT-PROJ-001";
        insert_feat_task(&conn, task_id, "low");
        let cfg = feat_codex_tier_cfg();

        // Real engine-cached defaults the FIXED recovery path will thread in.
        let project_default: Option<&str> = Some(HAIKU_MODEL);
        let user_default: Option<&str> = None;

        // Primary site: spawn-time resolution for the SAME inputs routes the task
        // to Codex via the `low` tier (HAIKU baseline → tier Haiku → `low`).
        let primary = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some(task_id),
            difficulty: Some("low"),
            project_default,
            user_default,
            primary_runner: Some(&cfg),
            ..Default::default()
        });
        assert_eq!(
            primary.provider_hint,
            Some(Provider::Codex),
            "anchor: primary site routes the non-high task to the Codex `low` tier"
        );

        // Recovery side: a RuntimeError at threshold must promote via the SAME
        // route. FIX-001 threads the engine-cached project/user defaults so the
        // recovery baseline derivation matches the primary site's.
        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed_for_runner(
            &conn,
            task_id,
            2,
            RunnerKind::Codex,
            &mut ctx,
            None,
            Some(&cfg),
            project_default,
            user_default,
        )
        .expect("escalate (project-default baseline)");

        assert!(
            result.is_some(),
            "recovery must promote: the `low` baseline-tier route matched (parity \
             with the primary site), so the Codex→Claude fallback fires"
        );
        assert_eq!(
            ctx.runner_overrides.get(task_id).copied(),
            Some(RunnerKind::Claude),
            "promotion target is always Claude; firing at all proves recovery \
             derived the SAME `low` tier the primary site did"
        );
        // target_model is a DISTINCT concern (non-high + claudeFallback unset →
        // OPUS baseline); asserted for completeness, not as the parity signal.
        assert_eq!(
            ctx.model_overrides.get(task_id).map(String::as_str),
            Some(OPUS_MODEL),
        );
    }

    // -- Sub-case B: baseline driven by user_default (the omission facet) --------
    {
        let (_tmp, conn) = setup_db();
        let task_id = "8d71d1f7-FEAT-USER-001";
        insert_feat_task(&conn, task_id, "low");
        let cfg = feat_codex_tier_cfg();

        // project absent; the baseline comes SOLELY from user_default — the field
        // current recovery omits entirely.
        let project_default: Option<&str> = None;
        let user_default: Option<&str> = Some(HAIKU_MODEL);

        let primary = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some(task_id),
            difficulty: Some("low"),
            project_default,
            user_default,
            primary_runner: Some(&cfg),
            ..Default::default()
        });
        assert_eq!(
            primary.provider_hint,
            Some(Provider::Codex),
            "anchor: user_default alone yields a HAIKU baseline → Codex `low` tier"
        );

        let mut ctx = IterationContext::new(8);
        // FIX-001 threads project_default/user_default through; user_default
        // alone now reaches the recovery baseline derivation.
        let result = escalate_task_model_if_needed_for_runner(
            &conn,
            task_id,
            2,
            RunnerKind::Codex,
            &mut ctx,
            None,
            Some(&cfg),
            project_default,
            user_default,
        )
        .expect("escalate (user-default baseline)");

        assert!(
            result.is_some(),
            "recovery must promote via the same `low` route — proving user_default \
             (not just project_default) reaches the recovery baseline derivation"
        );
        assert_eq!(
            ctx.runner_overrides.get(task_id).copied(),
            Some(RunnerKind::Claude),
        );
    }
}

// ── AC#3: recovery default-permutation matrix (additional cells) ─────────────
//
// The existing `recovery_baseline_tier_parity_non_high_codex` covers:
//   low x project_default=Some  (sub-case A)
//   low x user_default=Some     (sub-case B)
//
// The following tests fill the remaining matrix cells to prove parity
// between the primary site (`resolve_task_execution_target`) and the
// recovery path (`escalate_task_model_if_needed_for_runner`) across ALL
// four {difficulty high/low} x {defaults present/absent} combinations.

/// AC#3 — recovery matrix: high difficulty.
///
/// `compute_baseline_model(Some("high"), …) == OPUS_MODEL` regardless of
/// project/user defaults. Sub-case C verifies that even when
/// `project_default=SONNET` (standard tier), both paths route through the
/// OPUS/`high` tier — the high-difficulty short-circuit must not be shadowed
/// by a lower-tier project default. Sub-case E verifies the all-absent form.
#[test]
fn recovery_baseline_tier_parity_high_difficulty() {
    // --- Sub-case C: high difficulty + project_default=SONNET (standard tier) --
    {
        let (_tmp, conn) = setup_db();
        let task_id = "8d71d1f7-FEAT-HIGH-001";
        insert_feat_task(&conn, task_id, "high");
        let cfg = feat_codex_tier_cfg();

        // project_default is SONNET (standard tier), but high difficulty must
        // force OPUS regardless — `compute_baseline_model` short-circuits.
        let project_default: Option<&str> = Some(SONNET_MODEL);
        let user_default: Option<&str> = None;

        let primary = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some(task_id),
            difficulty: Some("high"),
            project_default,
            user_default,
            primary_runner: Some(&cfg),
            ..Default::default()
        });
        assert_eq!(
            primary.provider_hint,
            Some(Provider::Codex),
            "anchor: high difficulty → OPUS baseline → `high` tier → Codex, \
             even when project_default is a lower-tier model"
        );

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed_for_runner(
            &conn,
            task_id,
            2,
            RunnerKind::Codex,
            &mut ctx,
            None,
            Some(&cfg),
            project_default,
            user_default,
        )
        .expect("escalate (high difficulty, project_default=SONNET)");

        assert!(
            result.is_some(),
            "high difficulty → OPUS/high tier Codex route fires in recovery \
             (project_default is irrelevant when difficulty=high)"
        );
        assert_eq!(
            ctx.runner_overrides.get(task_id).copied(),
            Some(RunnerKind::Claude),
        );
        assert_eq!(
            ctx.model_overrides.get(task_id).map(String::as_str),
            Some(OPUS_MODEL),
            "high difficulty always promotes to OPUS regardless of project_default",
        );
    }

    // --- Sub-case E: high difficulty + no defaults --------------------------
    {
        let (_tmp, conn) = setup_db();
        let task_id = "8d71d1f7-FEAT-HIGH-002";
        insert_feat_task(&conn, task_id, "high");
        let cfg = feat_codex_tier_cfg();

        // No defaults at all: high still forces OPUS, so the `high` tier route fires.
        let project_default: Option<&str> = None;
        let user_default: Option<&str> = None;

        let primary = resolve_task_execution_target(&ModelResolutionContext {
            task_id: Some(task_id),
            difficulty: Some("high"),
            project_default,
            user_default,
            primary_runner: Some(&cfg),
            ..Default::default()
        });
        assert_eq!(
            primary.provider_hint,
            Some(Provider::Codex),
            "anchor: high difficulty + no defaults still routes through OPUS → `high` tier → Codex"
        );

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed_for_runner(
            &conn,
            task_id,
            2,
            RunnerKind::Codex,
            &mut ctx,
            None,
            Some(&cfg),
            project_default,
            user_default,
        )
        .expect("escalate (high difficulty, no defaults)");

        assert!(
            result.is_some(),
            "high difficulty + no defaults → OPUS baseline → `high` tier → promotes to Claude"
        );
        assert_eq!(
            ctx.runner_overrides.get(task_id).copied(),
            Some(RunnerKind::Claude),
        );
    }
}

/// AC#3 — recovery matrix: low difficulty, both defaults absent.
///
/// `compute_baseline_model(Some("low"), None, None, None) == None`.
/// `model_tier(None) == ModelTier::Default`, and
/// `primary_runner_baseline_tier_match` returns `None` immediately for the
/// `Default` tier. Result: BOTH the primary site (no `provider_hint`) and
/// the recovery path (no promotion) agree that no Codex tier route applies.
#[test]
fn recovery_baseline_tier_parity_low_difficulty_no_defaults() {
    let (_tmp, conn) = setup_db();
    let task_id = "8d71d1f7-FEAT-NODEF-001";
    insert_feat_task(&conn, task_id, "low");
    let cfg = feat_codex_tier_cfg();

    // No prd/project/user defaults → baseline=None → Default tier → no tier route.
    let project_default: Option<&str> = None;
    let user_default: Option<&str> = None;

    let primary = resolve_task_execution_target(&ModelResolutionContext {
        task_id: Some(task_id),
        difficulty: Some("low"),
        project_default,
        user_default,
        primary_runner: Some(&cfg),
        ..Default::default()
    });
    assert_eq!(
        primary.provider_hint, None,
        "anchor: low difficulty + no defaults → baseline=None → Default tier → \
         no baselineTierRoutes entry → no Codex provider_hint"
    );

    let mut ctx = IterationContext::new(8);
    let result = escalate_task_model_if_needed_for_runner(
        &conn,
        task_id,
        2,
        RunnerKind::Codex,
        &mut ctx,
        None,
        Some(&cfg),
        project_default,
        user_default,
    )
    .expect("escalate (low difficulty, no defaults)");

    // Parity: primary has no Codex hint → recovery fires no promotion.
    assert!(
        result.is_none(),
        "low difficulty + no defaults → baseline=None → Default tier → \
         no matching Codex route → no promotion"
    );
    assert!(
        ctx.runner_overrides.get(task_id).is_none(),
        "no promotion means no runner override inserted",
    );
}
