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

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::prompt::assembler::{PromptContext, assemble};
use task_mgr::loop_engine::prompt::sequential::sequential_roster;
use task_mgr::loop_engine::prompt::slot::slot_roster;
use task_mgr::loop_engine::prompt_sections::dependencies::{
    DEPENDENCIES_SECTION, build_dependency_section, dependencies_spec,
};
use task_mgr::models::Task;

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
    PromptContext {
        conn,
        task,
        task_files: &[],
        project_root: root,
        base_prompt_path: base,
        permission_mode: &DANGEROUS,
        steering_path: None,
        session_guidance: "",
        run_id: None,
        task_prefix: None,
        reorder_hint: None,
        batch_sibling_prds: None,
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

    // Only `dependencies` is migrated today, so it sits at index 0 in each
    // path's roster. As earlier-display sections migrate, this index will grow;
    // the durable invariant is that BOTH paths reach the section through the
    // shared spec, and each path owns its own independently-ordered Vec.
    assert_eq!(
        seq.iter().position(|s| s.name == DEPENDENCIES_SECTION),
        Some(0),
        "dependencies must occupy its legacy position in the sequential roster"
    );
    assert_eq!(
        slot.iter().position(|s| s.name == DEPENDENCIES_SECTION),
        Some(0),
        "dependencies must occupy its legacy position in the slot roster"
    );
}
