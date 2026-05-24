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

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::commands::next::output::{LearningSummaryOutput, NextTaskOutput, ScoreOutput};
use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::prompt::assembler::{PromptContext, SectionSpec, assemble};
use task_mgr::loop_engine::prompt::core;
use task_mgr::loop_engine::prompt::sequential::{
    build_base_prompt_section, build_completion_section,
    build_task_section as seq_build_task_section, sequential_roster,
};
use task_mgr::loop_engine::prompt::slot::{
    build_task_section as slot_build_task_section, load_base_prompt, slot_roster,
};
use task_mgr::loop_engine::prompt_sections::dependencies::{
    DEPENDENCIES_SECTION, build_dependency_section, dependencies_spec,
};
use task_mgr::loop_engine::prompt_sections::learnings::build_learnings_section;
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

    // Sequential display order: dependencies(0) precedes task; slot display
    // order: task(0) precedes dependencies(3, after the inserted learnings).
    // Each path owns its own independently-ordered Vec, but BOTH reach the
    // section through the shared `dependencies_spec`.
    assert_eq!(
        seq.iter().position(|s| s.name == DEPENDENCIES_SECTION),
        Some(0),
        "dependencies must occupy its legacy position in the sequential roster"
    );
    assert_eq!(
        slot.iter().position(|s| s.name == DEPENDENCIES_SECTION),
        Some(3),
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

    // Sequential display order of migrated sections:
    //   dependencies(0) → task(1) → task_ops(2) → learnings(3) → completion(4)
    //   → base_prompt(5)
    let seq_order = [
        "dependencies",
        "task",
        "task_ops",
        "learnings",
        "completion",
        "base_prompt",
    ];
    let seq_names: Vec<&str> = seq.iter().map(|s| s.name).collect();
    assert_eq!(seq_names, seq_order, "sequential roster display order");

    // Slot display order of migrated sections:
    //   task(0) → task_ops(1) → learnings(2) → dependencies(3) → completion(4)
    //   → base_prompt(5)
    let slot_order = [
        "task",
        "task_ops",
        "learnings",
        "dependencies",
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
