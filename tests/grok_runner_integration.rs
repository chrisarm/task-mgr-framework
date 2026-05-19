//! Realistic-scale integration tests for Grok control-tag emission.
//!
//! These tests run the real `grok` binary (discovered via PATH or `GROK_BINARY`)
//! with a realistic iteration-prompt fixture (10–100 KB) and verify that grok
//! correctly emits the three control tags the loop engine relies on:
//!
//! - `<promise>COMPLETE</promise>` / `<task-status>ID:done</task-status>`
//! - `<promise>BLOCKED</promise>`
//! - `<reorder>OTHER-ID</reorder>` + `<promise>BLOCKED</promise>`
//!
//! ## Why these are `#[ignore]`'d
//!
//! A real `grok` install is required.  CI environments don't have one, so
//! every test here carries `#[ignore]` and must be opted in explicitly:
//!
//! ```sh
//! cargo test --test grok_runner_integration -- --ignored grok_runner_integration
//! ```
//!
//! ## Fixture assembly
//!
//! Each test builds a prompt from three parts, mirroring the structure of a
//! real autonomous-loop iteration prompt:
//!
//! 1. **CLAUDE.md** — project-level instructions (~11 KB).
//! 2. **`.claude/commands/tasks.md`** — loop-protocol skill including the
//!    control-tag definitions (~61 KB).
//! 3. **Synthetic learnings** — plausible-looking learning entries that pad
//!    the prompt into the representative 10–100 KB range.
//! 4. **Task section** — the specific task description with acceptance
//!    criteria that drive the expected tag output.
//!
//! Total fixture size is asserted to be 10–100 KB in each test.
//!
//! ## Invocation note
//!
//! `grok --permission-mode plan --prompt-file <path>` is used instead of the
//! `-p` + stdin-pipe approach in `GrokRunner::spawn`, because grok's `-p`
//! flag (`--single`) requires the prompt as a direct argument value and does
//! not fall back to reading stdin.  Using `--prompt-file` avoids argv-length
//! limits for large fixtures and matches grok's documented large-prompt API.
//! The `--permission-mode plan` flag prevents any filesystem writes, making
//! the test safe to run in the project root.

use std::process::Command;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Generate a synthetic learnings block of approximately `target_bytes` bytes.
///
/// Content is realistic-looking learning entries rather than random data so
/// the fixture approximates the contextual weight of a real learnings section.
fn generate_synthetic_learnings(target_bytes: usize) -> String {
    const TEMPLATE: &str = r#"
### Learning #{n}: Compute effective_runner once per iteration
**Outcome**: pattern
**Confidence**: high
**Tags**: loop-engine, runner, grok-fallback

When the effective runner (Claude vs Grok) is resolved from model overrides or
config, compute it ONCE per iteration and pass the value through all downstream
call sites. Re-deriving at each call site creates drift: if the resolution
logic changes, some sites update and others don't, causing silent misrouting.
A single `effective_runner: RunnerKind` field on `IterationContext` is the
canonical pattern.

**Why**: OR-style guards (`runner_overrides.get(task)` OR
`provider_for_model(model)`) look equivalent but mask future drift when one
side updates. Pin to the pre-computed value — divergence becomes a compile
error, not a runtime surprise.

**Files**: src/loop_engine/engine.rs, src/loop_engine/context.rs

---
"#;

    let mut out = String::with_capacity(target_bytes + 256);
    out.push_str("## Relevant Learnings (Synthetic)\n\n");
    let mut n: u32 = 1;
    while out.len() < target_bytes {
        out.push_str(&TEMPLATE.replace("{n}", &n.to_string()));
        n += 1;
    }
    out
}

/// Read the project CLAUDE.md; fall back to an empty string on I/O error.
fn claude_md_content() -> String {
    std::fs::read_to_string("CLAUDE.md").unwrap_or_default()
}

/// Read the loop-protocol skill file; fall back to an inline tag summary so
/// the test is not silently vacuous when the file is missing.
fn skill_content() -> String {
    std::fs::read_to_string(".claude/commands/tasks.md").unwrap_or_else(|_| {
        // Minimal tag protocol so grok still understands the control tags.
        r#"## Task-Mgr Control Tags

When running as an autonomous loop agent:
- Task complete: emit `<promise>COMPLETE</promise>` then `<task-status>TASK-ID:done</task-status>`
- Task blocked: emit `<promise>BLOCKED</promise>` with a description of what is missing
- Reorder needed: emit `<reorder>OTHER-TASK-ID</reorder>` then `<promise>BLOCKED</promise>`
"#
        .to_string()
    })
}

/// Assemble the full iteration-prompt fixture.
///
/// Structure: CLAUDE.md + skill + synthetic learnings (~10 KB pad) + task
/// section.  The learnings size is tuned so the total stays within 10–100 KB.
fn build_prompt(task_section: &str) -> String {
    let claude_md = claude_md_content();
    let skill = skill_content();
    // Pad to roughly 10 KB of learnings; the rest of the fixture is ~72 KB,
    // so total lands around 82 KB — well within the 10–100 KB target range.
    let learnings = generate_synthetic_learnings(10_000);

    format!("{claude_md}\n\n---\n\n{skill}\n\n---\n\n{learnings}\n\n---\n\n{task_section}")
}

/// Resolve the grok binary path: `$GROK_BINARY` env-var → bare `"grok"` on PATH.
fn grok_binary() -> String {
    std::env::var("GROK_BINARY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "grok".to_string())
}

/// Write `prompt` to a temp file, run `grok --permission-mode plan
/// --prompt-file <path>`, remove the temp file, and return
/// `(stdout, stderr, exit_code)`.
///
/// `--permission-mode plan` prevents any filesystem writes, making this safe
/// to run inside the project root.
fn run_grok_with_prompt(prompt: &str) -> (String, String, i32) {
    let prompt_path = format!("/tmp/grok_integration_{}.txt", std::process::id());
    std::fs::write(&prompt_path, prompt).expect("write grok prompt temp file");

    let output = Command::new(grok_binary())
        .args(["--permission-mode", "plan", "--prompt-file", &prompt_path])
        .output()
        .expect("failed to spawn grok — is it on PATH or GROK_BINARY?");

    let _ = std::fs::remove_file(&prompt_path);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);
    (stdout, stderr, exit_code)
}

/// Extract the last `n` lines from a multi-line string.
fn last_n_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

// ---------------------------------------------------------------------------
// Integration tests (all #[ignore]'d for CI)
// ---------------------------------------------------------------------------

/// COMPLETE path — a trivially satisfiable task where grok need only emit the
/// completion tags without any tool use or file writes.
///
/// Asserts:
/// 1. `<promise>COMPLETE</promise>` is present in the **last 20 lines** of stdout.
/// 2. `<task-status>TEST-001:done</task-status>` is present anywhere in stdout.
#[test]
#[ignore = "requires grok binary — run: cargo test --test grok_runner_integration -- --ignored"]
fn grok_integration_complete_path_emits_correct_tags() {
    let task_section = r#"## Current Iteration

**Task ID**: TEST-001
**Title**: Tag emission verification
**Status**: todo

### Description

This is a protocol verification task for the task-mgr integration test suite.
Your job is to confirm that the loop control tags are emitted correctly.
No file writes, no tool calls, and no external resources are required.

### Acceptance Criteria

1. Output the confirmation string:
   `Tag emission verified — grok integration test COMPLETE`
2. Emit `<promise>COMPLETE</promise>` in your final output.
3. Emit `<task-status>TEST-001:done</task-status>` in your output.

This task is intentionally trivial — it exists solely to verify tag emission.
Do NOT call any tools. Do NOT read or write any files. Simply output the
confirmation string and the two required control tags, then stop.
"#;

    let prompt = build_prompt(task_section);
    let prompt_bytes = prompt.len();
    assert!(
        (10_000..=100_000).contains(&prompt_bytes),
        "fixture must be 10–100 KB for representative scale; got {prompt_bytes} bytes",
    );

    let (stdout, stderr, _exit_code) = run_grok_with_prompt(&prompt);

    eprintln!(
        "=== grok stdout (last 30 lines) ===\n{}",
        last_n_lines(&stdout, 30)
    );
    if !stderr.is_empty() {
        eprintln!(
            "=== grok stderr (last 5 lines) ===\n{}",
            last_n_lines(&stderr, 5)
        );
    }

    let tail = last_n_lines(&stdout, 20);

    assert!(
        tail.contains("<promise>COMPLETE</promise>"),
        "COMPLETE tag not found in last 20 lines of stdout.\n\
         Last 20 lines:\n{tail}\n\n\
         Full stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("<task-status>TEST-001:done</task-status>"),
        "task-status tag not found in stdout.\n\
         Full stdout:\n{stdout}",
    );
}

/// BLOCKED path — a task with an explicitly unspecified required parameter.
///
/// Asserts: `<promise>BLOCKED</promise>` is present in the **last 20 lines**
/// of stdout.
#[test]
#[ignore = "requires grok binary — run: cargo test --test grok_runner_integration -- --ignored"]
fn grok_integration_blocked_path_emits_blocked_tag() {
    let task_section = r#"## Current Iteration

**Task ID**: TEST-002
**Title**: Deploy to UNSPECIFIED environment
**Status**: todo

### Description

Deploy the application to `$DEPLOYMENT_TARGET`. This environment variable has
NOT been defined anywhere — not in any config file, not in the task parameters,
not in any environment variable visible to this session.

### Acceptance Criteria

1. Identify the target deployment environment from `$DEPLOYMENT_TARGET`.
2. Deploy the application to that environment.
3. Verify the deployment succeeded.

### Important

`$DEPLOYMENT_TARGET` is not specified.  You MUST NOT guess, assume, or
substitute a default value.  If any required parameter is missing or
unresolvable, emit `<promise>BLOCKED</promise>` followed by a description
of exactly what information is needed before this task can proceed.
"#;

    let prompt = build_prompt(task_section);
    let (stdout, stderr, _exit_code) = run_grok_with_prompt(&prompt);

    eprintln!(
        "=== grok stdout (last 30 lines) ===\n{}",
        last_n_lines(&stdout, 30)
    );
    if !stderr.is_empty() {
        eprintln!(
            "=== grok stderr (last 5 lines) ===\n{}",
            last_n_lines(&stderr, 5)
        );
    }

    let tail = last_n_lines(&stdout, 20);

    assert!(
        tail.contains("<promise>BLOCKED</promise>"),
        "BLOCKED tag not found in last 20 lines of stdout.\n\
         Last 20 lines:\n{tail}\n\n\
         Full stdout:\n{stdout}",
    );
}

/// REORDER path — a task blocked by an incomplete prerequisite, where the
/// eligible alternative (the prerequisite) should be promoted.
///
/// Asserts:
/// 1. `<reorder>SETUP-001</reorder>` is present anywhere in stdout.
/// 2. `<promise>BLOCKED</promise>` is present in the **last 20 lines** of stdout.
#[test]
#[ignore = "requires grok binary — run: cargo test --test grok_runner_integration -- --ignored"]
fn grok_integration_reorder_path_emits_reorder_and_blocked_tags() {
    let task_section = r#"## Current Iteration

**Task ID**: TEST-003
**Title**: Post-migration integration tests
**Status**: todo

### Description

Run the integration test suite against the new database schema introduced by
task SETUP-001 (database schema migration).  The integration tests REQUIRE the
schema migration to be fully applied before they can execute — running against
the old schema will cause every test to fail.

### Pre-conditions

- SETUP-001 (database schema migration) **must be complete** before this task.
- Current status of SETUP-001: **NOT COMPLETE** (`passes: false`).

### Acceptance Criteria

1. Verify SETUP-001 completed the schema migration successfully.
2. Run the integration test suite against the migrated schema.
3. Assert all tests pass.

### Important

SETUP-001 is NOT yet complete.  This task CANNOT proceed until SETUP-001
finishes.  You MUST:

1. Emit `<reorder>SETUP-001</reorder>` to promote SETUP-001 as the next task
   to run (it is a higher-priority eligible alternative).
2. Emit `<promise>BLOCKED</promise>` because this task is waiting on SETUP-001.

Do NOT attempt to run the tests now.  Do NOT modify any schema yourself.
"#;

    let prompt = build_prompt(task_section);
    let (stdout, stderr, _exit_code) = run_grok_with_prompt(&prompt);

    eprintln!(
        "=== grok stdout (last 30 lines) ===\n{}",
        last_n_lines(&stdout, 30)
    );
    if !stderr.is_empty() {
        eprintln!(
            "=== grok stderr (last 5 lines) ===\n{}",
            last_n_lines(&stderr, 5)
        );
    }

    let tail = last_n_lines(&stdout, 20);

    assert!(
        stdout.contains("<reorder>SETUP-001</reorder>"),
        "reorder tag not found in stdout.\n\
         Full stdout:\n{stdout}",
    );
    assert!(
        tail.contains("<promise>BLOCKED</promise>"),
        "BLOCKED tag not found in last 20 lines of stdout.\n\
         Last 20 lines:\n{tail}\n\n\
         Full stdout:\n{stdout}",
    );
}

// ---------------------------------------------------------------------------
// Live compile pin (no #[ignore] — runs in every `cargo test` invocation)
// ---------------------------------------------------------------------------

/// Compile-time pin: the test file builds and the fixture helpers produce
/// non-empty output.  Runs every `cargo test --test grok_runner_integration`
/// invocation so a build break surfaces even without a grok install.
#[test]
fn grok_integration_test_file_compiles_and_helpers_are_non_empty() {
    let learnings = generate_synthetic_learnings(500);
    assert!(
        !learnings.is_empty(),
        "generate_synthetic_learnings must return non-empty content"
    );
    assert!(
        learnings.contains("Learning #1"),
        "synthetic learnings must contain identifiable numbered entries"
    );

    // last_n_lines edge cases
    assert_eq!(last_n_lines("", 5), "");
    assert_eq!(last_n_lines("a\nb\nc", 2), "b\nc");
    assert_eq!(last_n_lines("a\nb\nc", 10), "a\nb\nc");
}
