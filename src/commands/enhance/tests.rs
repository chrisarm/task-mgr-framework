//! Unit tests for `task-mgr enhance` (agents, show, strip).
//!
//! Each test isolates the filesystem via [`tempfile::TempDir`] and uses an
//! explicit `cwd` parameter on the param structs so the harness never
//! depends on `std::env::current_dir()`.

use std::fs;
use std::time::SystemTime;

use tempfile::TempDir;

use super::templates::{
    EnhanceProfile, FULL_PROFILE_TOKEN, FULL_TEMPLATE, MARKER_BEGIN, MARKER_END,
    WORKFLOW_PROFILE_TOKEN, WORKFLOW_TEMPLATE,
};
use super::*;

fn mtime(path: &std::path::Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

// ─── templates ────────────────────────────────────────────────────────────

#[test]
fn full_template_strictly_contains_workflow_template() {
    // Acceptance: "Unit test: `full` profile strictly contains `workflow`
    // profile content (use length comparison + grep for a workflow-only
    // token)"
    assert!(
        FULL_TEMPLATE.len() > WORKFLOW_TEMPLATE.len(),
        "FULL must be longer than WORKFLOW; FULL={}, WORKFLOW={}",
        FULL_TEMPLATE.len(),
        WORKFLOW_TEMPLATE.len()
    );
    assert!(
        FULL_TEMPLATE.starts_with(WORKFLOW_TEMPLATE),
        "FULL must begin with WORKFLOW verbatim"
    );
    assert!(FULL_TEMPLATE.contains(WORKFLOW_PROFILE_TOKEN));
    assert!(FULL_TEMPLATE.contains(FULL_PROFILE_TOKEN));
    assert!(
        !WORKFLOW_TEMPLATE.contains(FULL_PROFILE_TOKEN),
        "workflow must NOT contain the full-only token"
    );
}

#[test]
fn profile_default_is_workflow() {
    assert_eq!(EnhanceProfile::default(), EnhanceProfile::Workflow);
}

// ─── enhance_show ─────────────────────────────────────────────────────────

#[test]
fn show_returns_rendered_body_and_writes_no_files() {
    // Acceptance: "`enhance show --profile workflow` prints to stdout; no
    // file writes (verify via mtime on any candidate file)"
    let dir = TempDir::new().unwrap();
    let claude_md = dir.path().join("CLAUDE.md");
    fs::write(&claude_md, "pre-existing content\n").unwrap();
    let before = mtime(&claude_md);

    let result = enhance_show(ShowParams {
        profile: EnhanceProfile::Workflow,
    })
    .unwrap();

    assert_eq!(result.kind, EnhanceKind::Show);
    assert_eq!(result.profile, Some(EnhanceProfile::Workflow));
    assert!(!result.dry_run);
    assert!(result.targets.is_empty());
    let body = result.rendered.expect("show must populate rendered");
    assert!(body.contains(WORKFLOW_PROFILE_TOKEN));
    assert!(!body.contains(FULL_PROFILE_TOKEN));

    // mtime of the candidate file must not have changed.
    assert_eq!(before, mtime(&claude_md));
}

#[test]
fn show_full_profile_includes_llm_guidelines() {
    let result = enhance_show(ShowParams {
        profile: EnhanceProfile::Full,
    })
    .unwrap();
    let body = result.rendered.unwrap();
    assert!(body.contains(WORKFLOW_PROFILE_TOKEN));
    assert!(body.contains(FULL_PROFILE_TOKEN));
}

// ─── enhance_agents — positive paths ─────────────────────────────────────

#[test]
fn agents_create_writes_marker_block_into_fresh_target() {
    // Acceptance: "Positive: `task-mgr enhance agents --create --target
    // /tmp/fresh.md --profile workflow` exits 0; /tmp/fresh.md exists with
    // `<!-- TASK_MGR:BEGIN -->` and `<!-- TASK_MGR:END -->` and the
    // workflow content between them"
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("fresh.md");

    let result = enhance_agents(AgentsParams {
        targets: vec![target.clone()],
        dry_run: false,
        create: true,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets.len(), 1);
    assert_eq!(result.targets[0].action, ActionTaken::Created);
    assert!(result.any_success());
    assert!(result.no_errors());

    let on_disk = fs::read_to_string(&target).unwrap();
    assert!(on_disk.contains(MARKER_BEGIN));
    assert!(on_disk.contains(MARKER_END));
    assert!(on_disk.contains(WORKFLOW_PROFILE_TOKEN));
    let bi = on_disk.find(MARKER_BEGIN).unwrap();
    let ei = on_disk.find(MARKER_END).unwrap();
    assert!(bi < ei, "BEGIN must precede END");
}

#[test]
fn agents_idempotent_second_run_produces_byte_identical_file() {
    // Acceptance: "Unit test: run `enhance agents --target X` twice on a
    // file containing user content before and after the marker block;
    // second run produces byte-identical file (idempotency)"
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    let prelude = "# Project notes\n\nSome user prelude.\n\n";
    let postlude = "\n## More user notes\n\nSecond paragraph by the user.\n";
    let initial = format!("{prelude}{MARKER_BEGIN}\nOLD CONTENT\n{MARKER_END}\n{postlude}");
    fs::write(&target, &initial).unwrap();

    let params = AgentsParams {
        targets: vec![target.clone()],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    };

    let _first = enhance_agents(params.clone()).unwrap();
    let after_first = fs::read_to_string(&target).unwrap();

    let _second = enhance_agents(params).unwrap();
    let after_second = fs::read_to_string(&target).unwrap();

    assert_eq!(after_first, after_second, "second run must be a no-op");
}

#[test]
fn agents_preserves_prelude_and_postlude_byte_for_byte() {
    // Acceptance: "Unit test: file with prelude + marker block + postlude →
    // after `enhance agents` with new content, prelude/postlude unchanged
    // byte-for-byte; marker contents replaced"
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("AGENTS.md");
    let prelude = "# Header\n\nLine A\nLine B\n\n";
    let postlude = "\n## Footer\n\nFinal user paragraph.\n";
    let initial = format!("{prelude}{MARKER_BEGIN}\nPLACEHOLDER\n{MARKER_END}\n{postlude}");
    fs::write(&target, &initial).unwrap();

    enhance_agents(AgentsParams {
        targets: vec![target.clone()],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    let updated = fs::read_to_string(&target).unwrap();
    // Prelude unchanged.
    assert!(
        updated.starts_with(prelude),
        "prelude must be preserved verbatim"
    );
    // Postlude unchanged.
    assert!(
        updated.ends_with(postlude),
        "postlude must be preserved verbatim"
    );
    // Old placeholder replaced.
    assert!(!updated.contains("PLACEHOLDER"));
    // New workflow body present between markers.
    assert!(updated.contains(WORKFLOW_PROFILE_TOKEN));
}

#[test]
fn agents_appends_block_when_target_has_no_markers() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    fs::write(&target, "# User notes\n\nSome content with no markers.\n").unwrap();

    let result = enhance_agents(AgentsParams {
        targets: vec![target.clone()],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets[0].action, ActionTaken::Appended);
    let updated = fs::read_to_string(&target).unwrap();
    assert!(updated.starts_with("# User notes\n\nSome content with no markers."));
    assert!(updated.contains(MARKER_BEGIN));
    assert!(updated.contains(MARKER_END));
}

#[test]
fn agents_default_targets_touch_both_claude_and_agents_md_when_present() {
    // Acceptance: "Both CLAUDE.md AND AGENTS.md exist (default targets) →
    // both updated."
    let dir = TempDir::new().unwrap();
    let claude_md = dir.path().join("CLAUDE.md");
    let agents_md = dir.path().join("AGENTS.md");
    fs::write(&claude_md, "claude original\n").unwrap();
    fs::write(&agents_md, "agents original\n").unwrap();

    let result = enhance_agents(AgentsParams {
        targets: vec![],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets.len(), 2);
    for outcome in &result.targets {
        assert_eq!(outcome.action, ActionTaken::Appended);
    }
    assert!(
        fs::read_to_string(&claude_md)
            .unwrap()
            .contains(MARKER_BEGIN)
    );
    assert!(
        fs::read_to_string(&agents_md)
            .unwrap()
            .contains(MARKER_BEGIN)
    );
}

#[test]
fn agents_explicit_target_does_not_process_defaults() {
    // Acceptance: "`--target docs/custom.md` (single explicit) → only that
    // file is touched; defaults NOT processed."
    let dir = TempDir::new().unwrap();
    let claude_md = dir.path().join("CLAUDE.md");
    let agents_md = dir.path().join("AGENTS.md");
    fs::write(&claude_md, "claude original\n").unwrap();
    fs::write(&agents_md, "agents original\n").unwrap();
    let custom = dir.path().join("custom.md");
    fs::write(&custom, "custom original\n").unwrap();

    let claude_before = mtime(&claude_md);
    let agents_before = mtime(&agents_md);

    enhance_agents(AgentsParams {
        targets: vec![custom.clone()],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(claude_before, mtime(&claude_md));
    assert_eq!(agents_before, mtime(&agents_md));
    assert!(fs::read_to_string(&custom).unwrap().contains(MARKER_BEGIN));
}

// ─── enhance_agents — negative / dry-run / skip ──────────────────────────

#[test]
fn agents_missing_files_without_create_skips_both_and_exit_2() {
    // Acceptance: "Unit test: neither CLAUDE.md nor AGENTS.md present in
    // CWD, `--create` not set → exit 2; stderr lists both skipped"
    let dir = TempDir::new().unwrap();

    let result = enhance_agents(AgentsParams {
        targets: vec![],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets.len(), 2);
    for outcome in &result.targets {
        assert_eq!(outcome.action, ActionTaken::Skipped);
    }
    // No targets succeeded → caller should exit 2.
    assert!(!result.any_success());
    // But there are no actual errors either — exit 2 vs 1 is purely about
    // "did anything succeed?"
    assert!(result.no_errors());
}

#[test]
fn agents_dry_run_writes_nothing_and_populates_preview() {
    // Acceptance: "`enhance show` and `enhance agents --dry-run` must NOT
    // produce any file mtime changes (test via before/after stat)"
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    fs::write(&target, "original\n").unwrap();
    let before = mtime(&target);

    let result = enhance_agents(AgentsParams {
        targets: vec![target.clone()],
        dry_run: true,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets[0].action, ActionTaken::DryRun);
    assert!(result.targets[0].preview.is_some());
    assert_eq!(before, mtime(&target), "dry-run must not change mtime");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "original\n",
        "dry-run must not modify file content"
    );
}

#[test]
fn agents_dry_run_does_not_exit_2_even_when_files_missing() {
    // Acceptance: "`--dry-run` → ... no exit-2 even if files missing"
    let dir = TempDir::new().unwrap();

    let result = enhance_agents(AgentsParams {
        targets: vec![],
        dry_run: true,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    // Even though all files are missing, dry-run should be a "preview"
    // operation. We allow `any_success` to be false but no errors.
    assert!(result.dry_run);
    assert!(result.no_errors());
}

#[test]
fn agents_unbalanced_markers_returns_error_outcome() {
    // Acceptance: "Unit test: unbalanced markers (BEGIN without END) in
    // target → returns Err; exit 2; stderr names the file"
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    let body = format!("prelude\n{MARKER_BEGIN}\norphan begin without end\n");
    fs::write(&target, &body).unwrap();

    let result = enhance_agents(AgentsParams {
        targets: vec![target.clone()],
        dry_run: false,
        create: false,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets[0].action, ActionTaken::Errored);
    let err = result.targets[0].error.as_ref().unwrap();
    assert!(
        err.contains(&target.display().to_string()),
        "error must name the file: {err}"
    );
    assert!(err.contains("unbalanced"));
    // File contents untouched.
    assert_eq!(fs::read_to_string(&target).unwrap(), body);
    assert!(!result.no_errors());
}

#[test]
fn agents_create_in_missing_parent_dir_returns_per_target_error() {
    // Acceptance: "Failure mode: target file path resolution fails (e.g.,
    // parent dir doesn't exist) → enhance reports per-target error in
    // EnhanceResult; other targets still attempted"
    let dir = TempDir::new().unwrap();
    let bad = dir.path().join("nonexistent-dir").join("file.md");
    let good = dir.path().join("custom.md");

    let result = enhance_agents(AgentsParams {
        targets: vec![bad.clone(), good.clone()],
        dry_run: false,
        create: true,
        profile: EnhanceProfile::Workflow,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets.len(), 2);
    assert_eq!(result.targets[0].action, ActionTaken::Errored);
    // Second target still attempted and succeeded (Created).
    assert_eq!(result.targets[1].action, ActionTaken::Created);
    assert!(good.exists());
}

// ─── enhance_strip ────────────────────────────────────────────────────────

#[test]
fn strip_removes_markers_and_preserves_user_content() {
    // Acceptance: "Unit test: `enhance strip` on file with markers + user
    // content → markers and block removed; user content preserved"
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    let prelude = "# User notes\n\nLine 1\nLine 2\n";
    let postlude = "## Tail\n\nLine 3\n";
    let initial =
        format!("{prelude}\n{MARKER_BEGIN}\ntask-mgr managed content\n{MARKER_END}\n{postlude}");
    fs::write(&target, &initial).unwrap();

    let result = enhance_strip(StripParams {
        targets: vec![target.clone()],
        dry_run: false,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets[0].action, ActionTaken::Stripped);
    let updated = fs::read_to_string(&target).unwrap();
    assert!(!updated.contains(MARKER_BEGIN));
    assert!(!updated.contains(MARKER_END));
    assert!(!updated.contains("task-mgr managed content"));
    assert!(updated.contains(prelude));
    assert!(updated.contains(postlude));
}

#[test]
fn strip_on_file_without_markers_is_noop_with_message() {
    // Acceptance: "`strip` on file without markers → no-op; print 'no
    // marker block found in <file>'; exit 0."
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    fs::write(&target, "user-only content\n").unwrap();
    let before = mtime(&target);

    let result = enhance_strip(StripParams {
        targets: vec![target.clone()],
        dry_run: false,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets[0].action, ActionTaken::NothingToStrip);
    assert_eq!(before, mtime(&target));
}

#[test]
fn strip_dry_run_does_not_write() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("CLAUDE.md");
    let initial = format!("pre\n{MARKER_BEGIN}\nbody\n{MARKER_END}\npost\n");
    fs::write(&target, &initial).unwrap();
    let before = mtime(&target);

    let result = enhance_strip(StripParams {
        targets: vec![target.clone()],
        dry_run: true,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(result.targets[0].action, ActionTaken::DryRun);
    assert_eq!(before, mtime(&target));
    assert_eq!(fs::read_to_string(&target).unwrap(), initial);
}

#[test]
fn strip_missing_file_is_skip_not_error() {
    let dir = TempDir::new().unwrap();
    let result = enhance_strip(StripParams {
        targets: vec![dir.path().join("does-not-exist.md")],
        dry_run: false,
        cwd: dir.path().to_path_buf(),
    })
    .unwrap();
    assert_eq!(result.targets[0].action, ActionTaken::Skipped);
}

// ─── source-code invariants (cause-fix for known-bad implementations) ────

#[test]
fn enhance_module_uses_only_marker_splice_write_atomic() {
    // Acceptance: "Negative: grep `src/commands/enhance/` for `std::fs::write`
    // returns zero hits — all writes through `marker_splice::write_atomic`"
    //
    // Mirrors the regression intent of the AC at the unit-test layer: walk
    // the enhance source files and assert no `std::fs::write(` call. We
    // also allow `fs::write` (re-imported) just in case — neither must
    // appear.
    use std::path::PathBuf;
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let enhance_dir = crate_root.join("src/commands/enhance");
    for entry in fs::read_dir(&enhance_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // tests.rs legitimately seeds fixture files via fs::write — the
        // invariant is that production code never reaches for the raw
        // write call.
        if fname == "tests.rs" {
            continue;
        }
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        // Defense-in-depth: also strip any inline #[cfg(test)] mod tests
        // block in case a future contributor moves tests inline.
        let prod = strip_test_module(&src);
        assert!(
            !prod.contains("std::fs::write("),
            "{} contains std::fs::write in non-test code",
            path.display()
        );
        assert!(
            !prod.contains("fs::write("),
            "{} contains fs::write in non-test code (use marker_splice::write_atomic instead)",
            path.display()
        );
    }
}

#[test]
fn enhance_module_uses_only_splice_block_for_markers() {
    // Acceptance: "CONTRACT: `splice_block(...)` is the only
    // marker-manipulation path; grep enhance/ for any other splice or
    // `marker_begin` literal"
    //
    // Concretely: the only place where the BEGIN / END literals
    // (`<!-- TASK_MGR:BEGIN -->`, `<!-- TASK_MGR:END -->`) appear in source
    // is `templates.rs` (the const definitions). Everywhere else must
    // reference `MARKER_BEGIN` / `MARKER_END`. This keeps the marker
    // strings single-source.
    use std::path::PathBuf;
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let enhance_dir = crate_root.join("src/commands/enhance");
    for entry in fs::read_dir(&enhance_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // templates.rs and tests.rs are allowed to mention the literals —
        // templates because that IS the definition, tests because they
        // construct fixtures.
        if fname == "templates.rs" || fname == "tests.rs" {
            continue;
        }
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        assert!(
            !src.contains("TASK_MGR:BEGIN"),
            "{} contains literal marker — use MARKER_BEGIN const",
            path.display()
        );
        assert!(
            !src.contains("TASK_MGR:END"),
            "{} contains literal marker — use MARKER_END const",
            path.display()
        );
    }
}

/// Heuristic: drop everything from the first `mod tests` line onward when
/// scanning prod source. Good enough for this codebase's convention where
/// tests live in an inline `#[cfg(test)] mod tests { ... }` block or in a
/// sibling `mod tests;` declaration.
fn strip_test_module(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.contains("mod tests") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
