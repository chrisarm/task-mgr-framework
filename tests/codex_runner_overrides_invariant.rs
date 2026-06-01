//! codex-runner invariant: no production code path inserts
//! `RunnerKind::Codex` into `IterationContext::runner_overrides`.
//!
//! Why this matters: `runner_overrides` is the channel the recovery /
//! overflow ladder uses to promote a task to a different provider AFTER it
//! has been spawned (e.g. Claude → Grok at rung 4). Codex is reached ONLY
//! via an explicit `primaryRunner` provider hint at the spawn-resolution
//! step (see `model::resolve_task_execution_target` →
//! `engine::resolve_effective_runner`). Inserting `RunnerKind::Codex` into
//! `runner_overrides` would turn Codex into a recovery / fallback target,
//! which would then run a `gpt-*` task through `CodexRunner` without the
//! route-gated startup binary probe (`check_codex_primary_binary`) ever
//! having fired — an unverified binary spawn mid-run.
//!
//! Approach: walk every `.rs` file under `src/` and assert that no line
//! both inserts into a `runner_overrides`-shaped expression AND mentions
//! `RunnerKind::Codex`. The check is intentionally textual because the
//! invariant is also textual — it's about which target_runner literals
//! the source code is allowed to write into the override channel.

use std::fs;
use std::path::Path;

/// Recursively collect every `.rs` file under `dir`, skipping `target/`.
fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_runner_override_inserts_codex() {
    let mut files = Vec::new();
    collect_rs_files(Path::new("src"), &mut files);
    assert!(
        !files.is_empty(),
        "no .rs files discovered under src/ — test environment is broken"
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let body = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Look for any PendingPromotion-style or direct override write
        // that pairs Codex with the override channel. Both shapes count:
        //   `target_runner: RunnerKind::Codex,`
        //   `runner_overrides.insert(..., RunnerKind::Codex)`
        // A line-wise scan is robust enough because all recovery sites in
        // src/loop_engine/recovery.rs write the variant on a single line.
        for (line_no, line) in body.lines().enumerate() {
            let mentions_codex = line.contains("RunnerKind::Codex");
            if !mentions_codex {
                continue;
            }
            let is_target_runner_assignment = line.contains("target_runner") && line.contains(':');
            let is_override_insert = line.contains("runner_overrides") && line.contains(".insert(");
            if is_target_runner_assignment || is_override_insert {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    line_no + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Invariant violated: production code inserts RunnerKind::Codex \
         into a runner_overrides channel. Codex is a primary-route-only target; \
         promoting it via the recovery / overflow ladder would bypass \
         the route-gated startup binary probe. Offending sites:\n  {}",
        offenders.join("\n  ")
    );
}
