//! Lint: exactly one `LIFECYCLE-EXCEPTION:` token is permitted in production
//! source code, and it must live in `src/commands/init/mod.rs`. The same file
//! is also the ONLY permitted production-code site for a raw
//! `UPDATE tasks SET status` SQL fragment outside `src/lifecycle/`.
//!
//! This test exists so that if the comment is removed (silently turning the
//! site back into an untracked raw SQL mutation) or if a new raw-SQL bypass
//! is introduced under a different file, `cargo test` catches it immediately.
//! Counting tokens alone (the original behavior) was insufficient: a new
//! orphan `UPDATE tasks SET status` site without the comment tag would still
//! pass — the second test below scans file contents directly.
//!
//! Exclusions: the `tests/` tree is not searched (this file would match
//! otherwise). Files named `test_utils.rs` are skipped. Content inside
//! `#[cfg(test)]` blocks is skipped via a brace-depth heuristic.
//!
//! Hermetic: no subprocess, no network, no fs writes.

use std::fs;
use std::path::{Path, PathBuf};

// ─── helpers ───────────────────────────────────────────────────────────────

/// Collect all `.rs` files under `dir`, excluding any whose path component
/// equals `test_utils.rs`.
fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rs_files_inner(dir, &mut out);
    out
}

fn collect_rs_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_inner(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            if file_name != "test_utils.rs" {
                out.push(path);
            }
        }
    }
}

/// Find line numbers (1-indexed) of `token` occurrences in `content`,
/// skipping any lines inside a `#[cfg(test)]` block.
///
/// Brace-depth heuristic: when we see `#[cfg(test)]`, we set a "skip next
/// block" flag. The block starts at the first `{` after the attribute and
/// ends when brace depth returns to where it started.
///
/// Single SSoT used by both `exactly_one_lifecycle_exception_token_in_src`
/// (which only needs the count) and
/// `no_orphan_raw_status_updates_outside_lifecycle` (which needs line numbers
/// for the diagnostic). The count test wraps with `.len()`.
fn find_token_lines_outside_cfg_test(content: &str, token: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut in_cfg_test_block = false;
    let mut pending_cfg_test = false; // saw #[cfg(test)], waiting for opening {
    let mut depth: usize = 0;
    let mut block_start_depth: usize = 0;

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;

        if !in_cfg_test_block && !pending_cfg_test && line.contains("#[cfg(test)]") {
            pending_cfg_test = true;
        }

        if pending_cfg_test || in_cfg_test_block {
            let opens = line.chars().filter(|&c| c == '{').count();
            let closes = line.chars().filter(|&c| c == '}').count();

            if pending_cfg_test && opens > 0 {
                // Block opens on this line.
                in_cfg_test_block = true;
                pending_cfg_test = false;
                block_start_depth = depth;
                depth += opens;
                depth = depth.saturating_sub(closes);
                continue; // don't count tokens on the opening line either
            } else if in_cfg_test_block {
                depth += opens;
                depth = depth.saturating_sub(closes);
                if depth <= block_start_depth {
                    in_cfg_test_block = false;
                    depth = block_start_depth;
                }
                continue; // skip tokens inside the block
            }
        }

        if line.contains(token) {
            hits.push(line_no);
        }
    }
    hits
}

/// Detects `UPDATE` statements on the `tasks` table (outside `#[cfg(test)]`
/// blocks) that also mutate the `status` column, even when the SQL is written
/// in forms the exact-token check misses:
///
///   - "UPDATE tasks SET notes = ?, status = 'todo' ...",
///   - split across continuation lines,
///   - `status = ?` or `SET ... status ...`.
///
/// Uses the same brace-depth `#[cfg(test)]` skipping logic so we never flag
/// test-only SQL. This is the semantic companion to the literal
/// `"UPDATE tasks SET status"` token scan.
fn find_status_column_mutations_outside_cfg_test(content: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut in_cfg_test_block = false;
    let mut pending_cfg_test = false;
    let mut depth: usize = 0;
    let mut block_start_depth: usize = 0;

    let lines: Vec<&str> = content.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;

        if !in_cfg_test_block && !pending_cfg_test && line.contains("#[cfg(test)]") {
            pending_cfg_test = true;
        }

        if pending_cfg_test || in_cfg_test_block {
            let opens = line.chars().filter(|&c| c == '{').count();
            let closes = line.chars().filter(|&c| c == '}').count();

            if pending_cfg_test && opens > 0 {
                in_cfg_test_block = true;
                pending_cfg_test = false;
                block_start_depth = depth;
                depth += opens;
                depth = depth.saturating_sub(closes);
                continue;
            } else if in_cfg_test_block {
                depth += opens;
                depth = depth.saturating_sub(closes);
                if depth <= block_start_depth {
                    in_cfg_test_block = false;
                    depth = block_start_depth;
                }
                continue;
            }
        }

        // Ignore anything after a line comment (//). This prevents the detector
        // from firing on explanatory comments that mention the old raw SQL.
        let code_part = if let Some(comment_start) = line.find("//") {
            &line[..comment_start]
        } else {
            line
        };

        let lower = code_part.to_lowercase();
        // Match the common ways we write UPDATE on the tasks table
        if lower.contains("update tasks")
            || lower.contains("update \"tasks\"")
            || lower.contains("update 'tasks'")
        {
            // Collect a small window to handle multi-line string literals
            let mut fragment = code_part.to_string();
            for j in 1..=3 {
                if let Some(next) = lines.get(idx + j) {
                    // Also respect // comments on continuation lines
                    let next_code = if let Some(c) = next.find("//") {
                        &next[..c]
                    } else {
                        next
                    };
                    fragment.push(' ');
                    fragment.push_str(next_code);
                    if fragment.contains(';') || fragment.matches('"').count() % 2 == 0 {
                        break;
                    }
                }
            }

            let frag_lower = fragment.to_lowercase();
            // Heuristic for "this statement assigns to the status column"
            if frag_lower.contains("status =")
                || frag_lower.contains("status=?")
                || frag_lower.contains("status = ?")
                || (frag_lower.contains("set") && frag_lower.contains("status"))
            {
                hits.push(line_no);
            }
        }
    }
    hits
}

// ─── test ──────────────────────────────────────────────────────────────────

#[test]
fn exactly_one_lifecycle_exception_token_in_src() {
    const TOKEN: &str = "LIFECYCLE-EXCEPTION:";
    const EXPECTED_FILE: &str = "src/commands/init/mod.rs";

    // Resolve `src/` relative to the workspace root (CARGO_MANIFEST_DIR
    // points to the crate root, which is the workspace root here).
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set when running under cargo test");
    let src_dir = PathBuf::from(&manifest_dir).join("src");
    assert!(
        src_dir.is_dir(),
        "src/ not found at {src_dir:?} — check CARGO_MANIFEST_DIR"
    );

    let files = collect_rs_files(&src_dir);
    assert!(
        !files.is_empty(),
        "collect_rs_files returned 0 files — check src_dir path"
    );

    let mut matches: Vec<String> = Vec::new();

    for file in &files {
        let content =
            fs::read_to_string(file).unwrap_or_else(|e| panic!("failed to read {file:?}: {e}"));
        let n = find_token_lines_outside_cfg_test(&content, TOKEN).len();
        if n > 0 {
            // Normalise to a forward-slash relative path for the assertion
            // message and the expected-file comparison.
            let rel = file
                .strip_prefix(&manifest_dir)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            // Remove a leading slash if present.
            let rel = rel.trim_start_matches('/').to_string();
            for _ in 0..n {
                matches.push(rel.clone());
            }
        }
    }

    assert_eq!(
        matches.len(),
        1,
        "expected exactly one '{TOKEN}' token in src/ (outside cfg(test) blocks), \
         found {}: {matches:?}\n\n\
         If you added a new raw-SQL bypass, route it through TaskLifecycle instead.\n\
         If you removed the init/mod.rs exception, restore the comment.",
        matches.len()
    );

    assert_eq!(
        matches[0], EXPECTED_FILE,
        "the single '{TOKEN}' token must be in {EXPECTED_FILE:?}, \
         but it was found in {:?}",
        matches[0]
    );
}

/// Content-scan companion to the token-count test above.
///
/// Performs *two* orthogonal scans on every `.rs` file under `src/` (outside
/// `src/lifecycle/` and outside `#[cfg(test)]` blocks):
///
/// 1. The original exact literal `"UPDATE tasks SET status"` (fast path).
/// 2. A semantic detector (`find_status_column_mutations_outside_cfg_test`)
///    that catches the bypass patterns the literal misses:
///      - `UPDATE tasks SET notes = ?, status = 'todo' ...`
///      - split across continuation lines
///      - `status = ?` or `SET ... status ...`
///
/// Lines inside `#[cfg(test)]` blocks are skipped using the brace-depth
/// heuristic. The only permitted production-code occurrence outside
/// `src/lifecycle/` is the single [`LIFECYCLE-EXCEPTION:`]-tagged site in
/// `src/commands/init/mod.rs`. Any other hit fails the test with `file:line`.
#[test]
fn no_orphan_raw_status_updates_outside_lifecycle() {
    const TOKEN: &str = "UPDATE tasks SET status";
    const LIFECYCLE_DIR_FRAG: &str = "src/lifecycle/";
    const EXPECTED_FILE: &str = "src/commands/init/mod.rs";

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set when running under cargo test");
    let src_dir = PathBuf::from(&manifest_dir).join("src");
    assert!(
        src_dir.is_dir(),
        "src/ not found at {src_dir:?} — check CARGO_MANIFEST_DIR"
    );

    let files = collect_rs_files(&src_dir);
    assert!(
        !files.is_empty(),
        "collect_rs_files returned 0 files — check src_dir path"
    );

    let mut offending: Vec<(String, usize)> = Vec::new();
    let mut expected_count: usize = 0;

    for file in &files {
        let rel = file
            .strip_prefix(&manifest_dir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let rel = rel.trim_start_matches('/').to_string();

        // Skip the lifecycle subsystem — those sites ARE the legitimate
        // mutation surface (TaskLifecycle's internal SQL).
        if rel.contains(LIFECYCLE_DIR_FRAG) {
            continue;
        }

        let content =
            fs::read_to_string(file).unwrap_or_else(|e| panic!("failed to read {file:?}: {e}"));

        // Pass 1: exact literal (the common pattern we write today)
        let hits = find_token_lines_outside_cfg_test(&content, TOKEN);
        for line_no in hits {
            if rel == EXPECTED_FILE {
                expected_count += 1;
            } else {
                offending.push((rel.clone(), line_no));
            }
        }

        // Pass 2: semantic detector for the bypass forms (H6 hardening).
        // IMPORTANT: only the *exact-token* pass manages expected_count for the
        // blessed file (we want to keep the original assertion semantics).
        // The semantic pass only ever contributes to offending (unless it's a
        // duplicate of a line the exact pass already reported).
        let status_hits = find_status_column_mutations_outside_cfg_test(&content);
        for line_no in status_hits {
            if rel != EXPECTED_FILE && !offending.iter().any(|(f, l)| f == &rel && *l == line_no) {
                offending.push((rel.clone(), line_no));
            }
            // If it *is* the blessed file we silently ignore it here — it was
            // already accounted for by the exact-token pass above.
        }
    }

    assert_eq!(
        expected_count, 1,
        "expected exactly one '{TOKEN}' occurrence in {EXPECTED_FILE:?} \
         (the LIFECYCLE-EXCEPTION-tagged bootstrap site), found {expected_count}",
    );

    if !offending.is_empty() {
        let formatted = offending
            .iter()
            .map(|(file, line)| format!("  {file}:{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "Found {} orphan raw '{TOKEN}' site(s) outside src/lifecycle/:\n{formatted}\n\n\
             Route the status write through `TaskLifecycle` (apply / try_claim / \
             reconcile_from_prd / repair_stale / decay_reset / recovery verbs) \
             or tag with LIFECYCLE-EXCEPTION:",
            offending.len(),
        );
    }
}
