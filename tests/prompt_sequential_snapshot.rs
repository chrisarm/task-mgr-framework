//! Phase A baseline snapshot test for `prompt::build_prompt` (which Phase A
//! moves to `prompt::sequential::build_prompt`).
//!
//! ## Why this test exists
//!
//! Phase A of the unify-execution-paths refactor splits the current
//! `loop_engine::prompt` module into `prompt::{core, sequential, slot}`.
//! The contract for that split is that the user-facing **sequential** prompt
//! must remain byte-identical for a fixed input fixture: no section
//! reordering, no whitespace drift, no header rename, no escaped-character
//! difference. This test pins that contract by comparing the assembled
//! prompt against a checked-in snapshot.
//!
//! The snapshot file (`tests/snapshots/prompt_sequential_v1.txt`) MUST be
//! captured BEFORE any prompt.rs edits land — that's the regression guard
//! the rest of Phase A leans on.
//!
//! ## Updating the snapshot
//!
//! Set `INSTA_UPDATE=1` (or `=true`) in the environment to regenerate the
//! snapshot from the current builder output:
//!
//! ```sh
//! INSTA_UPDATE=1 cargo test --test prompt_sequential_snapshot -- --nocapture
//! ```
//!
//! When the snapshot file does not yet exist, the test ALSO writes it (and
//! passes) — that's the one-shot bootstrap path. Subsequent runs without
//! `INSTA_UPDATE` enforce byte-equality.
//!
//! ## Determinism
//!
//! - Iteration is hard-coded to 1.
//! - Fresh `TempDir` DB per test (no persisted state).
//! - The fixture task has no `synergyWith`, `dependsOn`, or `touchesFiles`,
//!   so the synergy / dependency / source-context sections are empty.
//! - `steering_path` is `None` and `session_guidance` is `""`.
//! - Exactly one controlled learning row is inserted directly via
//!   `record_learning` so the learnings JSON block is deterministic.
//! - No escalation template file is created, so the escalation section is
//!   empty regardless of the resolved model tier.
//! - `permission_mode` is fixed at `Dangerous` so the tool-awareness block
//!   is the static "unrestricted" string.
//! - Per learning #896, this integration test cannot import the
//!   `pub(crate)` `loop_engine::test_utils` helpers; DB setup goes through
//!   the public `init::init` pathway and learning insertion uses the
//!   public `learnings::crud::record_learning` API.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::commands::init;
use task_mgr::db::open_connection;
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::prompt::{BuildPromptParams, build_prompt};
use task_mgr::models::{Confidence, LearningOutcome};

mod common;
use common::render_fixture_tmpl;

/// Path to the checked-in snapshot file, relative to the crate root.
const SNAPSHOT_PATH: &str = "tests/snapshots/prompt_sequential_v1.txt";

/// Stable base-prompt body used by the fixture. Kept inline (not loaded from
/// the project's real `prompt.md`) so the snapshot is independent of any
/// future edit to that file.
const BASE_PROMPT_BODY: &str = "# Agent Instructions\n\nImplement the task.\n";

/// Stable learning content. Single row keeps the recall ordering trivially
/// deterministic.
const LEARNING_TITLE: &str = "Snapshot baseline learning";
const LEARNING_CONTENT: &str = "Stable content used only by the sequential-prompt snapshot test.";

#[test]
fn sequential_prompt_matches_v1_snapshot() {
    let (_temp_dir, conn, base_prompt_path) = setup_fixture();

    let prompt = render_prompt(&conn, &base_prompt_path);

    compare_or_update_snapshot(&prompt);
}

/// Discriminator: the snapshot test must fail if a single character of
/// section ordering changes. We don't need a separate test for this — strict
/// byte-equality on the full assembled prompt automatically detects
/// reordering — but this test makes the discriminator explicit by checking
/// that the snapshot's section markers appear in the documented assembly
/// order. If the production builder ever silently reorders, both this test
/// AND `sequential_prompt_matches_v1_snapshot` will fail, which is the
/// intended belt-and-suspenders.
#[test]
fn snapshot_preserves_documented_section_order() {
    let snapshot_path = snapshot_abs_path();
    if !snapshot_path.exists() {
        // The companion test will create the snapshot on first run; skip
        // ordering verification until then so a brand-new checkout doesn't
        // double-fail.
        return;
    }

    let snapshot = fs::read_to_string(&snapshot_path)
        .unwrap_or_else(|e| panic!("read snapshot {}: {e}", snapshot_path.display()));

    // Markers in display order — see `prompt::build_prompt` assembly block.
    // (steering / session_guidance / reorder_hint omit when empty, so they
    // are NOT listed here; the fixture has them empty by design.)
    let ordered_markers = [
        "## Available Tools",
        "## Current Task",
        "## Task lifecycle",
        "## Relevant Learnings",
        "## Completing This Task",
        "## Key Decision Points",
        "# Agent Instructions",
    ];

    let mut cursor: usize = 0;
    for marker in ordered_markers {
        let idx = snapshot[cursor..].find(marker).unwrap_or_else(|| {
            panic!(
                "section marker {marker:?} missing or out of order in snapshot starting at byte {cursor}"
            )
        });
        cursor += idx + marker.len();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Absolute path to `tests/snapshots/prompt_sequential_v1.txt`.
fn snapshot_abs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_PATH)
}

/// Returns true when the runner has explicitly requested snapshot
/// regeneration via `INSTA_UPDATE=1` / `=true`.
fn snapshot_update_requested() -> bool {
    matches!(
        std::env::var("INSTA_UPDATE")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Initialize the fixture PRD into a temp dir, insert the controlled
/// learning row, and write a stable `prompt.md`. Returns the TempDir (must
/// outlive the connection), the open connection, and the base-prompt path.
fn setup_fixture() -> (TempDir, Connection, PathBuf) {
    let temp_dir = TempDir::new().expect("tempdir");

    // Render the fixture PRD into the temp dir and `init::init` it into a
    // fresh DB. `PrefixMode::Disabled` keeps task IDs literal (`SEQ-SNAP-001`).
    let prd_path = render_fixture_tmpl("sequential_prompt_v1.json.tmpl", temp_dir.path());
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .expect("init fixture PRD");

    let conn = open_connection(temp_dir.path()).expect("open db");

    // Insert the single controlled learning row. Tagging it with the task
    // ID and matching task-type prefix ensures `recall --for-task` surfaces
    // it deterministically as the sole result.
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: LEARNING_TITLE.to_string(),
        content: LEARNING_CONTENT.to_string(),
        task_id: Some("SEQ-SNAP-001".to_string()),
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["SEQ-SNAP-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params).expect("record learning");

    // Stable base-prompt body — pinned via `BASE_PROMPT_BODY`, NOT the
    // project's real `prompt.md`.
    let base_prompt_path = temp_dir.path().join("prompt.md");
    fs::write(&base_prompt_path, BASE_PROMPT_BODY).expect("write base prompt");

    (temp_dir, conn, base_prompt_path)
}

/// Build the prompt with deterministic params (iteration=1, no overrides,
/// dangerous permission mode).
fn render_prompt(conn: &Connection, base_prompt_path: &Path) -> String {
    let permission_mode = PermissionMode::Dangerous;
    let params = BuildPromptParams {
        dir: base_prompt_path.parent().expect("base prompt parent"),
        project_root: base_prompt_path.parent().expect("base prompt parent"),
        conn,
        after_files: &[],
        run_id: None,
        iteration: 1,
        reorder_hint: None,
        session_guidance: "",
        base_prompt_path,
        steering_path: None,
        verbose: false,
        default_model: None,
        project_default_model: None,
        user_default_model: None,
        task_prefix: None,
        batch_sibling_prds: &[],
        permission_mode: &permission_mode,
    };

    let result = build_prompt(&params)
        .expect("build_prompt returned Err")
        .expect("build_prompt returned None — fixture must yield exactly one task");

    assert_eq!(
        result.task_id, "SEQ-SNAP-001",
        "fixture should select SEQ-SNAP-001 (the only task)"
    );

    result.prompt
}

/// Compare the freshly built prompt against the on-disk snapshot.
///
/// Behavior:
/// - `INSTA_UPDATE=1` OR snapshot file missing → write the snapshot and pass.
/// - Otherwise → assert byte-equal; on mismatch panic with a unified-style
///   line-by-line diff that names the first differing line.
fn compare_or_update_snapshot(actual: &str) {
    let snapshot_path = snapshot_abs_path();
    let force_update = snapshot_update_requested();
    let missing = !snapshot_path.exists();

    if force_update || missing {
        if let Some(parent) = snapshot_path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("create snapshot dir {}: {e}", parent.display()));
        }
        fs::write(&snapshot_path, actual)
            .unwrap_or_else(|e| panic!("write snapshot {}: {e}", snapshot_path.display()));
        eprintln!(
            "[prompt_sequential_snapshot] {action} snapshot at {path} ({bytes} bytes)",
            action = if force_update {
                "regenerated"
            } else {
                "bootstrapped"
            },
            path = snapshot_path.display(),
            bytes = actual.len(),
        );
        return;
    }

    let expected = fs::read_to_string(&snapshot_path)
        .unwrap_or_else(|e| panic!("read snapshot {}: {e}", snapshot_path.display()));

    if expected == actual {
        return;
    }

    let diff = unified_diff(&expected, actual);
    panic!(
        "prompt::sequential::build_prompt output diverged from {path}.\n\
         To regenerate the snapshot intentionally, re-run with INSTA_UPDATE=1.\n\
         \n\
         --- expected ({expected_bytes} bytes)\n\
         +++ actual   ({actual_bytes} bytes)\n\
         {diff}",
        path = snapshot_path.display(),
        expected_bytes = expected.len(),
        actual_bytes = actual.len(),
    );
}

/// Produce a small unified-diff-style report for two strings.
///
/// This is a deliberately minimal implementation — line-by-line equality
/// with `-`/`+` markers and `=`-prefixed unchanged context around the first
/// divergence. Good enough to point a developer at the breakage; we don't
/// need the full Myers diff in CI output.
fn unified_diff(expected: &str, actual: &str) -> String {
    let exp_lines: Vec<&str> = expected.split_inclusive('\n').collect();
    let act_lines: Vec<&str> = actual.split_inclusive('\n').collect();

    // Find the first differing line (or the shorter side's length).
    let common_prefix = exp_lines
        .iter()
        .zip(act_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Show ~3 lines of context above the divergence + every differing line up
    // to a hard cap so a runaway diff doesn't drown CI logs.
    const CONTEXT: usize = 3;
    const MAX_DIFF_LINES: usize = 60;

    let ctx_start = common_prefix.saturating_sub(CONTEXT);
    let mut out = String::new();
    out.push_str(&format!(
        "@@ first divergence at line {} @@\n",
        common_prefix + 1
    ));

    for line in &exp_lines[ctx_start..common_prefix] {
        out.push_str("  ");
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }

    let exp_tail = &exp_lines[common_prefix..];
    let act_tail = &act_lines[common_prefix..];
    let cap = MAX_DIFF_LINES.min(exp_tail.len().max(act_tail.len()));

    for i in 0..cap {
        if let Some(line) = exp_tail.get(i) {
            out.push('-');
            out.push(' ');
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
        if let Some(line) = act_tail.get(i) {
            out.push('+');
            out.push(' ');
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
    }

    let total_diff = exp_tail.len().max(act_tail.len());
    if total_diff > MAX_DIFF_LINES {
        out.push_str(&format!(
            "... ({} more divergent lines truncated) ...\n",
            total_diff - MAX_DIFF_LINES
        ));
    }

    out
}
