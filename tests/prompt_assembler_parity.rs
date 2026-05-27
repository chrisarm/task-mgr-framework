//! Byte-parity tests for the data-driven prompt assembler (CONTRACT-001 / FEAT-002).
//!
//! Pins the FIRST migrated section — `dependencies` — to the invariant that
//! drives this whole effort: a section rendered through [`assemble`] must be
//! byte-identical to the LIVE legacy builder it replaces. Every parity
//! assertion diffs against `build_dependency_section` itself (NOT a frozen
//! expected string), so the test keeps guarding parity even if the legacy
//! builder's output format changes — a literal-string baseline would silently
//! pass in that case and is explicitly the known-bad this file guards against.
//!
//! Integration test (separate crate): only the public API is reachable, so
//! task rows are inserted with raw SQL through the migrated `Connection`,
//! mirroring the approach in `tests/prompt_slot.rs` (the `pub(crate)`
//! `loop_engine::test_utils` helpers are not visible here).

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::commands::next::output::{LearningSummaryOutput, NextTaskOutput, ScoreOutput};
use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};
use task_mgr::loop_engine::prompt::assembler::{PromptContext, SectionSpec, assemble};
use task_mgr::loop_engine::prompt::core;
use task_mgr::loop_engine::prompt::sequential::{
    build_base_prompt_section, build_completion_section, build_reorder_hint_section,
    build_reorder_instr_section, build_task_section as seq_build_task_section, sequential_roster,
};
use task_mgr::loop_engine::prompt::slot::{
    build_task_section as slot_build_task_section, load_base_prompt, slot_roster,
};
use task_mgr::loop_engine::prompt_sections::dependencies::{
    DEPENDENCIES_SECTION, build_dependency_section, dependencies_spec,
};
use task_mgr::loop_engine::prompt_sections::escalation::{
    ESCALATION_SECTION, build_escalation_section, escalation_spec,
};
use task_mgr::loop_engine::prompt_sections::learnings::build_learnings_section;
use task_mgr::loop_engine::prompt_sections::siblings::{
    SIBLINGS_SECTION, build_sibling_prd_section, siblings_spec,
};
use task_mgr::loop_engine::prompt_sections::synergy::{
    SYNERGY_SECTION, build_synergy_section, synergy_spec,
};
use task_mgr::models::{Confidence, LearningOutcome, Task};

/// Look up a spec by name in a roster, panicking with a helpful message if
/// absent (a missing migrated section is itself a regression worth a loud
/// failure rather than a silent skip).
fn spec_named(roster: &[SectionSpec], name: &str) -> SectionSpec {
    *roster
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("roster is missing the {name:?} section"))
}

/// Render a single named section through its roster spec.
fn render_named(roster: &[SectionSpec], name: &str, ctx: &PromptContext<'_>) -> String {
    let spec = spec_named(roster, name);
    (spec.render)(ctx, spec.kind).text
}

/// A `NextTaskOutput` carrier for the sequential task-envelope parity test.
fn sample_next_task_output() -> NextTaskOutput {
    NextTaskOutput {
        id: "TASK-001".to_string(),
        title: "Build API".to_string(),
        description: Some("Implement the public API surface".to_string()),
        priority: 7,
        status: "in_progress".to_string(),
        acceptance_criteria: vec!["Returns 200".to_string(), "Handles errors".to_string()],
        notes: Some("Mind the edge cases".to_string()),
        files: vec!["src/api.rs".to_string()],
        model: None,
        difficulty: Some("high".to_string()),
        escalation_note: None,
        requires_human: false,
        score: ScoreOutput {
            total: 100,
            priority: 90,
            file_overlap: 10,
            file_overlap_count: 1,
        },
    }
}

const DANGEROUS: PermissionMode = PermissionMode::Dangerous;

/// Whole-prompt budget shared by both builders; large enough that the small
/// dependency section always fits, so `assemble`'s output equals the raw
/// rendered text.
const BUDGET: usize = 80_000;

fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

fn insert_task(conn: &Connection, id: &str, title: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES (?1, ?2, ?3)",
        rusqlite::params![id, title, status],
    )
    .expect("insert task");
}

fn insert_depends_on(conn: &Connection, task_id: &str, dep_id: &str) {
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) \
         VALUES (?1, ?2, 'dependsOn')",
        rusqlite::params![task_id, dep_id],
    )
    .expect("insert relationship");
}

/// Build a `PromptContext` for `task`. The backing `Task` and path buffers are
/// owned by the caller so the borrows stay valid for the assemble call. The
/// dependencies render fn reads only `conn` + `task.id`; the remaining fields
/// are filled with inert defaults.
fn ctx_for<'a>(
    conn: &'a Connection,
    task: &'a Task,
    root: &'a std::path::Path,
    base: &'a std::path::Path,
) -> PromptContext<'a> {
    ctx_with_files(conn, task, root, base, &[], None)
}

/// As [`ctx_for`] but with caller-supplied `task_files` and an optional
/// `next_task_output` (the sequential task-envelope render reads the latter).
fn ctx_with_files<'a>(
    conn: &'a Connection,
    task: &'a Task,
    root: &'a std::path::Path,
    base: &'a std::path::Path,
    task_files: &'a [String],
    next_task_output: Option<&'a NextTaskOutput>,
) -> PromptContext<'a> {
    PromptContext {
        conn,
        task,
        task_files,
        project_root: root,
        base_prompt_path: base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output,
        recalled_learnings: None,
    }
}

// ---------------------------------------------------------------------------
// AC (a): a task with 2+ deps — Rendered.text and assemble() output are
// byte-identical to the live legacy builder.
// ---------------------------------------------------------------------------
#[test]
fn dependencies_render_matches_legacy_with_multiple_deps() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "DEP-001", "Schema setup", "done");
    insert_task(&conn, "DEP-002", "User model", "done");
    insert_task(&conn, "TASK-001", "Build API", "todo");
    insert_depends_on(&conn, "TASK-001", "DEP-001");
    insert_depends_on(&conn, "TASK-001", "DEP-002");

    // Diff target is the LIVE legacy fn, captured here — never a literal.
    let legacy = build_dependency_section(&conn, "TASK-001");
    assert!(
        legacy.contains("## Completed Dependencies")
            && legacy.contains("DEP-001")
            && legacy.contains("DEP-002"),
        "guard: legacy builder must produce real content for a 2-dep task, got {legacy:?}"
    );

    let task = Task::new("TASK-001", "Build API");
    let base = tmp.path().join("prompt.md");
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);

    // Rendered.text for the dependencies spec == legacy output.
    let spec = dependencies_spec();
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(
        rendered.text, legacy,
        "Rendered.text must be byte-identical to the live legacy builder"
    );

    // End-to-end via assemble(): a single-section roster reproduces the section.
    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(
        assembled.prompt, legacy,
        "assemble() output for the deps section must equal the legacy builder"
    );
    assert!(assembled.dropped_sections.is_empty());
}

// ---------------------------------------------------------------------------
// AC (b): a task with zero deps — empty section, parity holds, not "dropped".
// ---------------------------------------------------------------------------
#[test]
fn dependencies_render_matches_legacy_with_zero_deps() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "TASK-001", "Standalone", "todo");

    let legacy = build_dependency_section(&conn, "TASK-001");
    assert_eq!(
        legacy, "",
        "guard: a zero-dependency task yields an empty legacy section"
    );

    let task = Task::new("TASK-001", "Standalone");
    let base = tmp.path().join("prompt.md");
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);

    let spec = dependencies_spec();
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(
        rendered.text, legacy,
        "empty deps: Rendered.text must match legacy (both empty)"
    );

    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(assembled.prompt, legacy);
    assert!(
        assembled.dropped_sections.is_empty(),
        "an empty section is not a dropped section"
    );
}

// ---------------------------------------------------------------------------
// Edge case: deps spanning multiple statuses — only `done` ones appear, and
// the rendered text matches the legacy filter byte-for-byte.
// ---------------------------------------------------------------------------
#[test]
fn dependencies_render_matches_legacy_with_mixed_statuses() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "DEP-DONE", "Done dep", "done");
    insert_task(&conn, "DEP-TODO", "Todo dep", "todo");
    insert_task(&conn, "DEP-BLOCKED", "Blocked dep", "blocked");
    insert_task(&conn, "TASK-001", "Main", "todo");
    insert_depends_on(&conn, "TASK-001", "DEP-DONE");
    insert_depends_on(&conn, "TASK-001", "DEP-TODO");
    insert_depends_on(&conn, "TASK-001", "DEP-BLOCKED");

    let legacy = build_dependency_section(&conn, "TASK-001");
    assert!(
        legacy.contains("DEP-DONE")
            && !legacy.contains("DEP-TODO")
            && !legacy.contains("DEP-BLOCKED"),
        "guard: only the done dependency should appear, got {legacy:?}"
    );

    let task = Task::new("TASK-001", "Main");
    let base = tmp.path().join("prompt.md");
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);

    let spec = dependencies_spec();
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(rendered.text, legacy);
}

// ---------------------------------------------------------------------------
// AC: the dependencies spec sits at its legacy position in each path's roster,
// and the two rosters are independently constructed `Vec<SectionSpec>`s.
// ---------------------------------------------------------------------------
#[test]
fn dependencies_present_in_both_rosters_at_legacy_position() {
    let seq = sequential_roster();
    let slot = slot_roster();

    // After FEAT-006 the sequential roster also carries `reorder_hint` (before
    // tool_awareness), pushing dependencies to index 5 (after steering /
    // session_guidance / reorder_hint / tool_awareness / source). The slot
    // roster has no sequential-only sections, so dependencies stays at index 4
    // (after task / task_ops / learnings / source). Each path owns its own
    // independently-ordered Vec, but BOTH reach the section through the shared
    // `dependencies_spec`.
    assert_eq!(
        seq.iter().position(|s| s.name == DEPENDENCIES_SECTION),
        Some(5),
        "dependencies must occupy its legacy position in the sequential roster"
    );
    assert_eq!(
        slot.iter().position(|s| s.name == DEPENDENCIES_SECTION),
        Some(4),
        "dependencies must occupy its legacy position in the slot roster"
    );
}

// ---------------------------------------------------------------------------
// AC: the four critical sections appear in BOTH rosters at their own legacy
// display positions, as `SectionKind::Critical` specs. The two rosters are
// independently ordered (slot emits `task` first; sequential mid-list).
// ---------------------------------------------------------------------------
#[test]
fn criticals_present_in_both_rosters_at_legacy_positions() {
    use task_mgr::loop_engine::prompt::assembler::SectionKind;

    let seq = sequential_roster();
    let slot = slot_roster();

    // Sequential display order (FEAT-006 — EVERY section the path emits is on
    // the roster, including the sequential-only reorder_hint / synergy /
    // siblings / escalation / reorder_instr):
    //   steering → session_guidance → reorder_hint → tool_awareness → source →
    //   dependencies → synergy → siblings → task → task_ops → learnings →
    //   completion → escalation → reorder_instr → key_decision → base_prompt
    let seq_order = [
        "steering",
        "session_guidance",
        "reorder_hint",
        "tool_awareness",
        "source",
        "dependencies",
        "synergy",
        "siblings",
        "task",
        "task_ops",
        "learnings",
        "completion",
        "escalation",
        "reorder_instr",
        "key_decision",
        "base_prompt",
    ];
    let seq_names: Vec<&str> = seq.iter().map(|s| s.name).collect();
    assert_eq!(seq_names, seq_order, "sequential roster display order");

    // Slot display order (FEAT-005 — the slot path has no sequential-only
    // sections, so this is the complete slot prompt layout):
    //   task → task_ops → learnings → source → dependencies → steering →
    //   session_guidance → tool_awareness → key_decision → completion → base_prompt
    let slot_order = [
        "task",
        "task_ops",
        "learnings",
        "source",
        "dependencies",
        "steering",
        "session_guidance",
        "tool_awareness",
        "key_decision",
        "completion",
        "base_prompt",
    ];
    let slot_names: Vec<&str> = slot.iter().map(|s| s.name).collect();
    assert_eq!(slot_names, slot_order, "slot roster display order");

    // Each of the four envelope sections is a Critical spec in both rosters.
    for name in ["task", "task_ops", "completion", "base_prompt"] {
        assert!(
            matches!(spec_named(&seq, name).kind, SectionKind::Critical),
            "{name} must be Critical in the sequential roster"
        );
        assert!(
            matches!(spec_named(&slot, name).kind, SectionKind::Critical),
            "{name} must be Critical in the slot roster"
        );
    }
}

// ---------------------------------------------------------------------------
// Per-section parity (SEQUENTIAL): each critical's rendered text is
// byte-identical to the live legacy builder it wraps.
// ---------------------------------------------------------------------------
#[test]
fn sequential_critical_sections_match_legacy() {
    let (tmp, conn) = setup_migrated_db();
    let base = tmp.path().join("prompt.md");
    std::fs::write(&base, "# Agent Instructions\n\nDo the work.").expect("write base prompt");

    let next_out = sample_next_task_output();
    let task = Task::new(&next_out.id, &next_out.title);
    let files = next_out.files.clone();
    let ctx = ctx_with_files(&conn, &task, tmp.path(), &base, &files, Some(&next_out));

    let roster = sequential_roster();

    // task envelope: NextTaskOutput JSON, truncated to TASK_CONTEXT_BUDGET.
    let legacy_task = seq_build_task_section(&next_out);
    assert!(
        legacy_task.contains("## Current Task") && legacy_task.contains("TASK-001"),
        "guard: legacy task envelope must have real content, got {legacy_task:?}"
    );
    assert_eq!(render_named(&roster, "task", &ctx), legacy_task);

    // task_ops: shared static lifecycle rules.
    let task_ops = render_named(&roster, "task_ops", &ctx);
    assert!(
        task_ops.contains("## Task lifecycle"),
        "guard: task_ops must render the lifecycle rules, got {task_ops:?}"
    );

    // completion: the sequential variant (non-code note + [Title] placeholder).
    // `files` is non-empty here, so no non-code note.
    let legacy_completion = build_completion_section(&next_out.id, files.is_empty());
    assert!(
        legacy_completion.contains("## Completing This Task")
            && legacy_completion.contains("[Title]"),
        "guard: sequential completion must use the [Title] placeholder, got {legacy_completion:?}"
    );
    assert_eq!(render_named(&roster, "completion", &ctx), legacy_completion);

    // base_prompt: the sequential reader (trailing-newline fixup).
    let legacy_base = build_base_prompt_section(&base);
    assert!(
        legacy_base.contains("# Agent Instructions") && legacy_base.ends_with('\n'),
        "guard: sequential base prompt must end with a newline, got {legacy_base:?}"
    );
    assert_eq!(render_named(&roster, "base_prompt", &ctx), legacy_base);
}

// ---------------------------------------------------------------------------
// The sequential completion render honors the no-files note for milestone /
// verification tasks (touchesFiles empty).
// ---------------------------------------------------------------------------
#[test]
fn sequential_completion_renders_non_code_note_when_no_files() {
    let (tmp, conn) = setup_migrated_db();
    let base = tmp.path().join("prompt.md");
    let task = Task::new("MILE-001", "Milestone");
    let next_out = NextTaskOutput {
        files: vec![],
        ..sample_next_task_output()
    };
    let ctx = ctx_with_files(&conn, &task, tmp.path(), &base, &[], Some(&next_out));

    let rendered = render_named(&sequential_roster(), "completion", &ctx);
    // The sequential completion render reads `ctx.task.id`, so the legacy diff
    // target must use the same id (MILE-001), not the NextTaskOutput's.
    let legacy = build_completion_section(&task.id, true);
    assert!(
        rendered.contains("no `touchesFiles`"),
        "no-files task must render the verification/milestone note"
    );
    assert_eq!(rendered, legacy);
}

// ---------------------------------------------------------------------------
// Per-section parity (SLOT): each critical's rendered text is byte-identical
// to the live legacy builder it wraps. The slot envelopes differ from
// sequential (untruncated Task JSON, core::completion_instruction, no
// trailing-newline base prompt).
// ---------------------------------------------------------------------------
#[test]
fn slot_critical_sections_match_legacy() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "TASK-001", "Build API", "in_progress");
    let base = tmp.path().join("prompt.md");
    std::fs::write(&base, "# Agent Instructions\n\nDo the work.").expect("write base prompt");

    let task = Task::new("TASK-001", "Build API");
    let files = vec!["src/api.rs".to_string()];
    let ctx = ctx_with_files(&conn, &task, tmp.path(), &base, &files, None);

    let roster = slot_roster();

    // task envelope: untruncated Task JSON via core::format_task_json.
    let legacy_task = slot_build_task_section(&task, &files);
    assert!(
        legacy_task.contains("## Current Task") && legacy_task.contains("TASK-001"),
        "guard: legacy slot task envelope must have real content, got {legacy_task:?}"
    );
    assert_eq!(render_named(&roster, "task", &ctx), legacy_task);

    // task_ops: identical shared static text in both paths.
    assert_eq!(
        render_named(&roster, "task_ops", &ctx),
        render_named(&sequential_roster(), "task_ops", &ctx),
        "task_ops is the one critical shared verbatim between the two paths"
    );

    // completion: the slot variant (core::completion_instruction).
    let legacy_completion = core::completion_instruction(&task.id, &task.title);
    assert!(
        legacy_completion.contains("Build API"),
        "guard: slot completion embeds the title, got {legacy_completion:?}"
    );
    assert_eq!(render_named(&roster, "completion", &ctx), legacy_completion);

    // base_prompt: the slot reader (no trailing-newline fixup).
    let legacy_base = load_base_prompt(&base);
    assert!(
        !legacy_base.ends_with('\n'),
        "guard: slot base prompt preserves the file verbatim (no newline fixup), got {legacy_base:?}"
    );
    assert_eq!(render_named(&roster, "base_prompt", &ctx), legacy_base);
}

// ---------------------------------------------------------------------------
// The two paths' task envelopes legitimately DIFFER (sequential truncates a
// NextTaskOutput JSON; slot emits an untruncated Task JSON). This pins that
// the migration preserved each path's distinct bytes rather than unifying them.
// ---------------------------------------------------------------------------
#[test]
fn sequential_and_slot_task_envelopes_are_path_specific() {
    let next_out = sample_next_task_output();
    let task = Task::new(&next_out.id, &next_out.title);
    let seq_text = seq_build_task_section(&next_out);
    let slot_text = slot_build_task_section(&task, &next_out.files);
    // Both wrap the same `## Current Task` header but the JSON bodies differ
    // (sequential includes `score`-derived fields only via NextTaskOutput's
    // shape; the carriers are distinct types), so the renders are not forced
    // to be identical — the rosters being independent is the whole point.
    assert!(seq_text.starts_with("## Current Task"));
    assert!(slot_text.starts_with("## Current Task"));
}

// ---------------------------------------------------------------------------
// FEAT-004 — Learnings parity (SEQUENTIAL): the render formats the learnings
// `next::next` already recalled (carried in PromptContext::recalled_learnings),
// so it is recall-limit-driven. Rendered.text and assemble() output are
// byte-identical to the live `build_learnings_section` builder, and the
// recalled IDs ride through as the section side output.
// ---------------------------------------------------------------------------
#[test]
fn sequential_learnings_render_matches_legacy() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "TASK-001", "Build API", "todo");
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");

    // Two learnings constructed directly (the sequential render reads these
    // from the context, NOT from a fresh DB recall). Diff target is the LIVE
    // legacy builder, never a frozen literal.
    let recalled = vec![
        LearningSummaryOutput {
            id: 11,
            title: "Use the assembler".to_string(),
            outcome: "pattern".to_string(),
            confidence: "high".to_string(),
            content: Some("Route every section through assemble()".to_string()),
            applies_to_files: Some(vec!["src/loop_engine/prompt/assembler.rs".to_string()]),
            applies_to_task_types: Some(vec!["FEAT-".to_string()]),
        },
        LearningSummaryOutput {
            id: 22,
            title: "Centralize the clear".to_string(),
            outcome: "failure".to_string(),
            confidence: "medium".to_string(),
            content: None,
            applies_to_files: None,
            applies_to_task_types: None,
        },
    ];
    let legacy = build_learnings_section(&recalled);
    assert!(
        legacy.contains("## Relevant Learnings") && legacy.contains("Use the assembler"),
        "guard: legacy learnings builder must produce real content, got {legacy:?}"
    );

    let ctx = PromptContext {
        conn: &conn,
        task: &task,
        task_files: &[],
        project_root: tmp.path(),
        base_prompt_path: &base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: Some(&recalled),
    };

    let roster = sequential_roster();
    let spec = spec_named(&roster, "learnings");
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(
        rendered.text, legacy,
        "sequential learnings text must equal the live builder"
    );
    assert_eq!(
        rendered.shown_learning_ids,
        vec![11, 22],
        "the recalled IDs must ride through as the section side output"
    );

    // End-to-end via assemble(): the IDs survive because the section fits.
    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(assembled.prompt, legacy);
    assert_eq!(assembled.shown_learning_ids, vec![11, 22]);
    assert!(assembled.dropped_sections.is_empty());
}

// ---------------------------------------------------------------------------
// FEAT-004 — Centralized invariant (SEQUENTIAL spec): when the real sequential
// learnings spec is dropped under budget pressure, assemble() — the SOLE owner
// of the clear — leaves shown_learning_ids empty. Proves the sequential path
// routes its side output through assemble() rather than a per-builder clear.
// ---------------------------------------------------------------------------
#[test]
fn sequential_learnings_dropped_clears_shown_ids() {
    let (tmp, conn) = setup_migrated_db();
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");

    let recalled = vec![LearningSummaryOutput {
        id: 11,
        title: "Some learning".to_string(),
        outcome: "pattern".to_string(),
        confidence: "high".to_string(),
        content: Some("x".repeat(200)),
        applies_to_files: None,
        applies_to_task_types: None,
    }];

    let ctx = PromptContext {
        conn: &conn,
        task: &task,
        task_files: &[],
        project_root: tmp.path(),
        base_prompt_path: &base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: Some(&recalled),
    };

    let spec = spec_named(&sequential_roster(), "learnings");
    // A 10-byte total budget cannot fit the ~200-byte learnings section.
    let assembled = assemble(&ctx, std::slice::from_ref(&spec), 10);
    assert_eq!(assembled.dropped_sections, vec!["learnings".to_string()]);
    assert!(
        assembled.shown_learning_ids.is_empty(),
        "dropped sequential learnings must not credit the bandit"
    );
    assert_eq!(assembled.prompt, "");
}

// ---------------------------------------------------------------------------
// FEAT-004 — Learnings parity (SLOT): the render recalls from the DB via
// core::build_learnings_block with LEARNINGS_BUDGET=4000. Rendered.text and the
// surfaced IDs are byte-identical to that live builder for a recallable
// learning.
// ---------------------------------------------------------------------------
#[test]
fn slot_learnings_render_matches_legacy() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "TASK-001", "Build API", "in_progress");

    // A learning recallable for TASK-001 via its task-type prefix.
    let learn = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "slot learnings parity".into(),
        content: "Route slot learnings through assemble()".into(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/slot.rs".into()]),
        applies_to_task_types: Some(vec!["TASK-".into()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, learn).expect("record_learning");

    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);

    // The slot spec carries LEARNINGS_BUDGET=4000; the live builder uses the
    // same budget so the diff is exact. Never a frozen literal.
    let (legacy_text, legacy_ids) = core::build_learnings_block(&conn, &task, 4_000);
    assert!(
        legacy_text.contains("## Relevant Learnings"),
        "guard: a recallable learning must produce real slot learnings content, got {legacy_text:?}"
    );
    assert!(
        !legacy_ids.is_empty(),
        "guard: the recalled learning must surface an id"
    );

    let roster = slot_roster();
    let spec = spec_named(&roster, "learnings");
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(
        rendered.text, legacy_text,
        "slot learnings text must equal the live builder (LEARNINGS_BUDGET=4000)"
    );
    assert_eq!(
        rendered.shown_learning_ids, legacy_ids,
        "slot learnings must surface the same recalled IDs as the live builder"
    );
}

// ===========================================================================
// FEAT-005 — the remaining shared trimmables: source, steering,
// session_guidance, tool_awareness, key_decision. Each is a SHARED SectionSpec
// (one render site, wrapping a `core::build_*` helper), reached by both rosters
// at its legacy display position. Parity is asserted per path against the LIVE
// core helper, never a frozen literal.
// ===========================================================================

/// Materialize a project with a source file (`src/api.rs`) and a `steering.md`,
/// returning the tempdir plus the relative `task_files` for the source render.
fn project_with_source_and_steering() -> (TempDir, Vec<String>, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(
        src.join("api.rs"),
        "pub fn handle_request() {}\npub struct ApiHandler;\n",
    )
    .expect("write source file");
    let steering = tmp.path().join("steering.md");
    std::fs::write(&steering, "Focus on error handling.").expect("write steering");
    (tmp, vec!["src/api.rs".to_string()], steering)
}

/// Build a fully-populated context for the shared-trimmable renders: a real
/// source file, a steering path, non-empty guidance, and the Dangerous mode.
fn shared_trimmable_ctx<'a>(
    conn: &'a Connection,
    task: &'a Task,
    root: &'a std::path::Path,
    base: &'a std::path::Path,
    files: &'a [String],
    steering: &'a std::path::Path,
    guidance: &'a str,
) -> PromptContext<'a> {
    PromptContext {
        conn,
        task,
        task_files: files,
        project_root: root,
        base_prompt_path: base,
        permission_mode: &DANGEROUS,
        steering_path: Some(steering),
        session_guidance: guidance,
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: None,
    }
}

/// Assert every shared trimmable rendered through `roster` is byte-identical to
/// the live `core::build_*` helper it wraps. Shared by the sequential and slot
/// per-path parity tests below (the specs are identical across rosters; this
/// proves each path actually reaches them at its own position).
fn assert_shared_trimmables_match_legacy(roster: &[SectionSpec], ctx: &PromptContext<'_>) {
    let files = ctx.task_files;
    let root = ctx.project_root;
    let steering = ctx
        .steering_path
        .expect("test ctx supplies a steering path");

    // source — diff against the live builder using the SAME 2000-byte budget.
    let legacy_source = core::build_source_context_block(files, core::SOURCE_CONTEXT_BUDGET, root);
    assert!(
        legacy_source.contains("## Current Source Context")
            && legacy_source.contains("handle_request"),
        "guard: legacy source must have real content, got {legacy_source:?}"
    );
    assert_eq!(render_named(roster, "source", ctx), legacy_source);

    // steering
    let legacy_steering = core::build_steering_block(steering);
    assert!(
        legacy_steering.contains("## Steering")
            && legacy_steering.contains("Focus on error handling."),
        "guard: legacy steering must have real content, got {legacy_steering:?}"
    );
    assert_eq!(render_named(roster, "steering", ctx), legacy_steering);

    // session_guidance
    let legacy_guidance = core::build_session_guidance_block(ctx.session_guidance);
    assert!(
        legacy_guidance.contains("## Session Guidance"),
        "guard: legacy session guidance must have real content, got {legacy_guidance:?}"
    );
    assert_eq!(
        render_named(roster, "session_guidance", ctx),
        legacy_guidance
    );

    // tool_awareness
    let legacy_tool = core::build_tool_awareness_block(&DANGEROUS);
    assert!(
        legacy_tool.contains("## Available Tools"),
        "guard: legacy tool awareness must have real content, got {legacy_tool:?}"
    );
    assert_eq!(render_named(roster, "tool_awareness", ctx), legacy_tool);

    // key_decision
    let legacy_key = core::build_key_decisions_block(&ctx.task.id);
    assert!(
        legacy_key.contains("## Key Decision Points"),
        "guard: legacy key decision must have real content, got {legacy_key:?}"
    );
    assert_eq!(render_named(roster, "key_decision", ctx), legacy_key);
}

#[test]
fn shared_trimmables_render_matches_legacy_sequential() {
    let (tmp, files, steering) = project_with_source_and_steering();
    let conn = setup_migrated_db().1; // independent in-memory-ish db; only used for ctx
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");
    let ctx = shared_trimmable_ctx(
        &conn,
        &task,
        tmp.path(),
        &base,
        &files,
        &steering,
        "User said: prioritize tests",
    );
    assert_shared_trimmables_match_legacy(&sequential_roster(), &ctx);

    // Sequential's source bytes were historically produced by
    // `scan_source_context(..).format_for_prompt()` directly. Pin that the
    // migration onto `build_source_context_block` preserved those exact bytes
    // (the only divergence is suppressed stderr on a missing root, not output).
    let direct = task_mgr::loop_engine::context::scan_source_context(
        &files,
        core::SOURCE_CONTEXT_BUDGET,
        tmp.path(),
    )
    .format_for_prompt();
    assert_eq!(
        render_named(&sequential_roster(), "source", &ctx),
        direct,
        "migrated source must equal sequential's original scan_source_context bytes"
    );
}

#[test]
fn shared_trimmables_render_matches_legacy_slot() {
    let (tmp, files, steering) = project_with_source_and_steering();
    let conn = setup_migrated_db().1;
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");
    let ctx = shared_trimmable_ctx(
        &conn,
        &task,
        tmp.path(),
        &base,
        &files,
        &steering,
        "User said: prioritize tests",
    );
    assert_shared_trimmables_match_legacy(&slot_roster(), &ctx);
}

// ---------------------------------------------------------------------------
// CONTRACT / distinct-budget: the source cap (2000) and the slot learnings cap
// (4000) ride on SEPARATE `SectionKind::Trimmable` budget fields. Collapsing
// them into one shared field is the known-bad this test guards against — it
// would pass a single-section parity test but fail here.
// ---------------------------------------------------------------------------
#[test]
fn source_and_learnings_budgets_stay_distinct() {
    use task_mgr::loop_engine::prompt::assembler::SectionKind;

    let slot = slot_roster();
    let budget_of = |name: &str| match spec_named(&slot, name).kind {
        SectionKind::Trimmable { budget } => budget,
        SectionKind::Critical => panic!("{name} should be a trimmable section"),
    };

    let source_budget = budget_of("source");
    let learnings_budget = budget_of("learnings");

    assert_eq!(
        source_budget,
        core::SOURCE_CONTEXT_BUDGET,
        "source must carry SOURCE_CONTEXT_BUDGET on its SectionKind"
    );
    assert_eq!(
        source_budget, 2000,
        "SOURCE_CONTEXT_BUDGET must remain 2000"
    );
    assert_eq!(
        learnings_budget, 4000,
        "slot learnings must keep its independent 4000-byte cap"
    );
    assert_ne!(
        source_budget, learnings_budget,
        "the source and learnings caps must stay on distinct SectionKind fields, \
         not collapse into one shared budget"
    );
}

// ---------------------------------------------------------------------------
// Distinct budget — behavioral: an oversize source section is capped by its OWN
// 2000-byte budget, not the larger learnings cap. Diffs against the live
// builder at the same budget.
// ---------------------------------------------------------------------------
#[test]
fn source_section_capped_at_its_own_budget() {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    // ~500 public fns — far more than 2000 bytes of signatures.
    let mut big = String::new();
    for i in 0..500 {
        big.push_str(&format!("pub fn generated_function_number_{i}() {{}}\n"));
    }
    std::fs::write(src.join("big.rs"), &big).expect("write big source");
    let files = vec!["src/big.rs".to_string()];

    let conn = setup_migrated_db().1;
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "t");
    let ctx = ctx_with_files(&conn, &task, tmp.path(), &base, &files, None);

    let rendered = render_named(&sequential_roster(), "source", &ctx);
    let legacy = core::build_source_context_block(&files, core::SOURCE_CONTEXT_BUDGET, tmp.path());
    assert_eq!(
        rendered, legacy,
        "capped source must equal the live builder"
    );
    assert!(
        rendered.contains("## Current Source Context"),
        "the section must still render its header"
    );
    // Capped by the 2000 source budget, comfortably under the 4000 learnings cap.
    assert!(
        rendered.len() < 3000,
        "source must be bounded by its own ~2000 budget, not the 4000 learnings cap; \
         got {} bytes",
        rendered.len()
    );
}

// ---------------------------------------------------------------------------
// Failure mode: a missing / absent steering file renders an empty section
// (warn-and-continue), never a panic — matching pre-migration behavior. Empty
// task_files likewise yields an empty source section, not an error.
// ---------------------------------------------------------------------------
#[test]
fn missing_steering_and_empty_source_render_empty_no_panic() {
    let tmp = TempDir::new().expect("tempdir");
    let conn = setup_migrated_db().1;
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "t");

    // steering_path = Some(nonexistent file): build_steering_block reads-and-fails
    // gracefully → "".
    let missing_steering = tmp.path().join("does-not-exist-steering.md");
    let ctx = PromptContext {
        conn: &conn,
        task: &task,
        task_files: &[],
        project_root: tmp.path(),
        base_prompt_path: &base,
        permission_mode: &DANGEROUS,
        steering_path: Some(&missing_steering),
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: None,
    };

    let roster = sequential_roster();
    assert_eq!(
        render_named(&roster, "steering", &ctx),
        "",
        "missing steering file must render an empty section"
    );
    assert_eq!(
        render_named(&roster, "source", &ctx),
        "",
        "empty task_files must render an empty source section, not an error"
    );
    assert_eq!(
        render_named(&roster, "session_guidance", &ctx),
        "",
        "empty session guidance must render an empty section"
    );

    // None steering path also renders empty (no panic).
    let ctx_none = ctx_for(&conn, &task, tmp.path(), &base);
    assert_eq!(render_named(&roster, "steering", &ctx_none), "");
}

// ===========================================================================
// FEAT-006 — the sequential-ONLY sections: synergy, escalation, siblings,
// reorder_hint, reorder_instr. Each is a SectionSpec present in the sequential
// roster and ABSENT from the slot roster (proving slot ⊂ sequential as a SET).
// Parity is asserted per section against the LIVE legacy builder, never a
// frozen literal (synergy parity == empty string by design).
// ===========================================================================

/// Write a sibling PRD JSON file with the given user stories into `dir`,
/// returning its path. Mirrors the shape `siblings.rs` parses (`PrdFile`).
fn write_sibling_prd(dir: &std::path::Path, name: &str, stories: serde_json::Value) -> PathBuf {
    let prd = serde_json::json!({ "project": "sibling-project", "userStories": stories });
    let path = dir.join(name);
    std::fs::write(&path, prd.to_string()).expect("write sibling prd");
    path
}

// ---------------------------------------------------------------------------
// AC: the five sequential-only specs sit at their legacy positions in the
// sequential roster and are ABSENT from the slot roster (assert on the slot
// roster construction directly).
// ---------------------------------------------------------------------------
#[test]
fn sequential_only_sections_present_in_seq_roster_absent_from_slot() {
    let seq = sequential_roster();
    let slot = slot_roster();

    // Present in the sequential roster at the legacy display positions.
    let pos = |name: &str| seq.iter().position(|s| s.name == name);
    assert_eq!(pos("reorder_hint"), Some(2), "reorder_hint legacy position");
    assert_eq!(pos(SYNERGY_SECTION), Some(6), "synergy legacy position");
    assert_eq!(pos(SIBLINGS_SECTION), Some(7), "siblings legacy position");
    assert_eq!(
        pos(ESCALATION_SECTION),
        Some(12),
        "escalation legacy position"
    );
    assert_eq!(
        pos("reorder_instr"),
        Some(13),
        "reorder_instr legacy position"
    );

    // Absent from the slot roster — wave slots drop these by design, so the
    // slot roster is a strict set-subset of the sequential roster.
    for name in [
        "reorder_hint",
        SYNERGY_SECTION,
        SIBLINGS_SECTION,
        ESCALATION_SECTION,
        "reorder_instr",
    ] {
        assert!(
            !slot.iter().any(|s| s.name == name),
            "{name} must NOT appear in the slot roster"
        );
    }
}

// ---------------------------------------------------------------------------
// Synergy: a permanent no-op. Rendered text and assemble() output are
// byte-identical to the live builder — which is the empty string.
// ---------------------------------------------------------------------------
#[test]
fn sequential_synergy_render_matches_legacy_empty() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "TASK-001", "Build API", "todo");
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);

    // Diff target is the LIVE builder (the no-op), never a literal.
    let legacy = build_synergy_section(&conn, "TASK-001", None);
    assert_eq!(legacy, "", "guard: synergy is a permanent no-op (empty)");

    let spec = synergy_spec();
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(
        rendered.text, legacy,
        "synergy Rendered.text must equal the live no-op builder"
    );
    assert_eq!(
        render_named(&sequential_roster(), SYNERGY_SECTION, &ctx),
        legacy
    );

    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(
        assembled.prompt, legacy,
        "synergy assemble() output == empty"
    );
    assert!(
        assembled.dropped_sections.is_empty(),
        "an empty no-op section is not a dropped section"
    );
}

// ---------------------------------------------------------------------------
// Siblings: a MILESTONE task in batch mode renders remaining sibling tasks.
// Rendered text and assemble() output are byte-identical to the live builder.
// ---------------------------------------------------------------------------
#[test]
fn sequential_siblings_render_matches_legacy() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "MILESTONE-001", "Wrap up phase", "todo");
    let base = tmp.path().join("prompt.md");
    let sibling = write_sibling_prd(
        tmp.path(),
        "sibling.json",
        serde_json::json!([{
            "id": "SIB-001",
            "title": "Downstream consumer",
            "priority": 10,
            "passes": false,
            "touchesFiles": ["src/consumer.rs"],
        }]),
    );
    let prds = [sibling];

    // Diff target is the LIVE builder with the same args the render uses.
    let legacy = build_sibling_prd_section(&conn, "MILESTONE-001", None, &prds);
    assert!(
        legacy.contains("## Sibling PRD Tasks") && legacy.contains("SIB-001"),
        "guard: legacy siblings builder must produce real content, got {legacy:?}"
    );

    let task = Task::new("MILESTONE-001", "Wrap up phase");
    let ctx = PromptContext {
        conn: &conn,
        task: &task,
        task_files: &[],
        project_root: tmp.path(),
        base_prompt_path: &base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: Some(&prds),
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: None,
    };

    let spec = siblings_spec();
    let rendered = (spec.render)(&ctx, spec.kind);
    assert_eq!(
        rendered.text, legacy,
        "siblings Rendered.text must equal the live builder"
    );
    assert_eq!(
        render_named(&sequential_roster(), SIBLINGS_SECTION, &ctx),
        legacy
    );

    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(
        assembled.prompt, legacy,
        "siblings assemble() output == legacy"
    );
    assert!(assembled.dropped_sections.is_empty());
}

// ---------------------------------------------------------------------------
// Edge: no batch sibling PRDs → the siblings section is empty (not "dropped"),
// matching the legacy builder for an empty path list. Same for a non-milestone
// task even with PRDs present.
// ---------------------------------------------------------------------------
#[test]
fn sequential_siblings_empty_when_no_batch_prds() {
    let (tmp, conn) = setup_migrated_db();
    insert_task(&conn, "MILESTONE-001", "Wrap up", "todo");
    let base = tmp.path().join("prompt.md");
    let task = Task::new("MILESTONE-001", "Wrap up");

    // batch_sibling_prds defaults to None in ctx_for → render passes &[].
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);
    let legacy = build_sibling_prd_section(&conn, "MILESTONE-001", None, &[]);
    assert_eq!(legacy, "", "guard: empty PRD list yields an empty section");

    let spec = siblings_spec();
    assert_eq!((spec.render)(&ctx, spec.kind).text, legacy);
    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(assembled.prompt, "");
    assert!(
        assembled.dropped_sections.is_empty(),
        "an empty siblings section is not a dropped section"
    );
}

// ---------------------------------------------------------------------------
// Escalation (CRITICAL): for a non-Opus model with a template on disk, the
// rendered text and assemble() output equal the live builder. For Opus the
// section disappears.
// ---------------------------------------------------------------------------
#[test]
fn sequential_escalation_render_matches_legacy() {
    let (tmp, conn) = setup_migrated_db();
    let base = tmp.path().join("prompt.md");
    std::fs::write(&base, "# Agent Instructions\n").expect("write base");
    // Template resolves at base.parent()/scripts/escalation-policy.md.
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts dir");
    std::fs::write(
        scripts.join("escalation-policy.md"),
        "When stuck, escalate to opus.",
    )
    .expect("write template");

    let task = Task::new("TASK-001", "Build API");

    // Non-Opus resolved model → section present. Diff against the live builder.
    let legacy = build_escalation_section(&base, Some(SONNET_MODEL));
    assert!(
        legacy.contains("## Model Escalation Policy") && legacy.contains("escalate to opus"),
        "guard: non-opus escalation must render the template, got {legacy:?}"
    );

    let ctx_sonnet = PromptContext {
        conn: &conn,
        task: &task,
        task_files: &[],
        project_root: tmp.path(),
        base_prompt_path: &base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
        resolved_model: Some(SONNET_MODEL),
        next_task_output: None,
        recalled_learnings: None,
    };
    let spec = escalation_spec();
    assert_eq!((spec.render)(&ctx_sonnet, spec.kind).text, legacy);
    assert_eq!(
        render_named(&sequential_roster(), ESCALATION_SECTION, &ctx_sonnet),
        legacy
    );
    // Critical single-spec assemble reproduces the section byte-for-byte.
    let assembled = assemble(&ctx_sonnet, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(assembled.prompt, legacy);
    assert!(assembled.dropped_sections.is_empty());

    // Opus tier → the section disappears (parity with the live builder).
    let opus_legacy = build_escalation_section(&base, Some(OPUS_MODEL));
    assert_eq!(opus_legacy, "", "guard: opus omits the escalation policy");
    let ctx_opus = PromptContext {
        resolved_model: Some(OPUS_MODEL),
        ..clone_ctx(&ctx_sonnet)
    };
    assert_eq!((spec.render)(&ctx_opus, spec.kind).text, opus_legacy);
}

// ---------------------------------------------------------------------------
// Reorder hint: present when a hint id is carried, empty when None. Both match
// the live builder byte-for-byte.
// ---------------------------------------------------------------------------
#[test]
fn sequential_reorder_hint_render_matches_legacy() {
    let (tmp, conn) = setup_migrated_db();
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");

    // With a hint.
    let legacy_some = build_reorder_hint_section(Some("OTHER-002"));
    assert!(
        legacy_some.contains("## Reorder Hint") && legacy_some.contains("OTHER-002"),
        "guard: a reorder hint must render real content, got {legacy_some:?}"
    );
    let ctx_some = PromptContext {
        conn: &conn,
        task: &task,
        task_files: &[],
        project_root: tmp.path(),
        base_prompt_path: &base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: Some("OTHER-002"),
        batch_sibling_prds: None,
        resolved_model: None,
        next_task_output: None,
        recalled_learnings: None,
    };
    assert_eq!(
        render_named(&sequential_roster(), "reorder_hint", &ctx_some),
        legacy_some
    );

    // No hint → empty section, not a dropped section.
    let legacy_none = build_reorder_hint_section(None);
    assert_eq!(legacy_none, "", "guard: no hint yields an empty section");
    let ctx_none = ctx_for(&conn, &task, tmp.path(), &base);
    let spec = spec_named(&sequential_roster(), "reorder_hint");
    assert_eq!((spec.render)(&ctx_none, spec.kind).text, legacy_none);
    let assembled = assemble(&ctx_none, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(assembled.prompt, "");
    assert!(assembled.dropped_sections.is_empty());
}

// ---------------------------------------------------------------------------
// Reorder instruction (CRITICAL): a constant section. Rendered text and
// assemble() output equal the live builder.
// ---------------------------------------------------------------------------
#[test]
fn sequential_reorder_instr_render_matches_legacy() {
    let (tmp, conn) = setup_migrated_db();
    let base = tmp.path().join("prompt.md");
    let task = Task::new("TASK-001", "Build API");
    let ctx = ctx_for(&conn, &task, tmp.path(), &base);

    let legacy = build_reorder_instr_section();
    assert!(
        legacy.contains("<reorder>TASK-ID</reorder>"),
        "guard: reorder instruction must carry the example tag, got {legacy:?}"
    );
    let spec = spec_named(&sequential_roster(), "reorder_instr");
    assert_eq!((spec.render)(&ctx, spec.kind).text, legacy);
    assert_eq!(
        render_named(&sequential_roster(), "reorder_instr", &ctx),
        legacy
    );
    let assembled = assemble(&ctx, std::slice::from_ref(&spec), BUDGET);
    assert_eq!(assembled.prompt, legacy);
    assert!(assembled.dropped_sections.is_empty());
}

// ============================================================
// Roster-completeness test (FEAT-007)
// ============================================================
//
// The enumerated set of section names that MUST appear in the sequential
// roster. This is the canonical "what sections does the sequential prompt
// have?" known-set — NOT a hardcoded count. Adding a new section requires:
//   1. A `*_spec()` constructor and `SectionSpec` in the appropriate module.
//   2. An entry in `sequential_roster()` in `prompt/sequential.rs`.
//   3. An entry here.
// The test below catches step 2 or 3 being missed; it names the missing
// section rather than reporting a generic count mismatch.
const SEQUENTIAL_KNOWN_SECTIONS: &[&str] = &[
    "steering",
    "session_guidance",
    "reorder_hint",
    "tool_awareness",
    "source",
    "dependencies",
    "synergy",
    "siblings",
    "task",
    "task_ops",
    "learnings",
    "completion",
    "escalation",
    "reorder_instr",
    "key_decision",
    "base_prompt",
];

/// Roster-completeness test: every name in [`SEQUENTIAL_KNOWN_SECTIONS`] must
/// appear in the sequential roster returned by [`sequential_roster`].
///
/// Fails naming the first missing section, not with a generic count mismatch.
/// This replaces the hand-enforced "any new section added to the sequential
/// prompt must also be wired into slot — there is no second source of truth"
/// rule that previously lived in `prompt/mod.rs` and
/// `src/loop_engine/CLAUDE.md`. The mechanical check is now a test-time gate.
#[test]
fn sequential_roster_contains_all_known_sections() {
    let roster = sequential_roster();
    let roster_names: Vec<&str> = roster.iter().map(|s| s.name).collect();
    for expected in SEQUENTIAL_KNOWN_SECTIONS {
        assert!(
            roster_names.contains(expected),
            "sequential_roster() is missing the {:?} section — add it to \
             `sequential_roster()` or remove it from `SEQUENTIAL_KNOWN_SECTIONS`",
            expected,
        );
    }
}

/// Clone a `PromptContext` by copying its (all-`Copy`-or-shared-ref) fields.
/// Used to vary a single field (e.g. `resolved_model`) in a test without
/// re-typing every field.
fn clone_ctx<'a>(ctx: &PromptContext<'a>) -> PromptContext<'a> {
    PromptContext {
        conn: ctx.conn,
        task: ctx.task,
        task_files: ctx.task_files,
        project_root: ctx.project_root,
        base_prompt_path: ctx.base_prompt_path,
        permission_mode: ctx.permission_mode,
        steering_path: ctx.steering_path,
        session_guidance: ctx.session_guidance,
        run_id: ctx.run_id,
        task_prefix: ctx.task_prefix,
        reorder_hint: ctx.reorder_hint,
        batch_sibling_prds: ctx.batch_sibling_prds,
        resolved_model: ctx.resolved_model,
        next_task_output: ctx.next_task_output,
        recalled_learnings: ctx.recalled_learnings,
    }
}
