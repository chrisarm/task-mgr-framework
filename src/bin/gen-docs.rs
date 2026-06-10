//! Regenerates the MODELS block in `.claude/commands/tasks.md` from the
//! canonical source-of-truth constants in `src/loop_engine/model.rs`.
//!
//! Usage:
//!   cargo run --bin gen-docs              # rewrite the doc in-place
//!   cargo run --bin gen-docs -- --check   # exit 1 with a diff if stale
//!
//! See `src/loop_engine/model.rs` for why this exists.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use task_mgr::util::marker_splice;

const MODELS_BEGIN: &str = "<!-- MODELS:BEGIN -->";
const MODELS_END: &str = "<!-- MODELS:END -->";
const EXPECTED_MODEL_CONSTS: &[&str] =
    &["FABLE_MODEL", "OPUS_MODEL", "SONNET_MODEL", "HAIKU_MODEL"];

fn main() -> ExitCode {
    let check_mode = std::env::args().any(|a| a == "--check");

    let root = match repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gen-docs: could not locate repo root: {e}");
            return ExitCode::from(2);
        }
    };
    let model_rs = root.join("src/loop_engine/model.rs");
    let targets = [
        root.join(".claude/commands/tasks.md"),
        root.join(".claude/commands/plan-tasks.md"),
    ];

    let block = match render_block(&model_rs) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "gen-docs: failed to render block from {}: {e}",
                model_rs.display()
            );
            return ExitCode::from(2);
        }
    };

    let mut any_updated = false;

    for target in &targets {
        let current = match fs::read_to_string(target) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "gen-docs: could not read {}: {e}. Skipping.",
                    target.display()
                );
                continue;
            }
        };

        // gen-docs requires exactly one marker pair — validate before splicing.
        let begin_count = current.matches(MODELS_BEGIN).count();
        let end_count = current.matches(MODELS_END).count();
        if begin_count == 0 || end_count == 0 {
            eprintln!(
                "gen-docs: expected {MODELS_BEGIN} and {MODELS_END} markers in {}; \
                 found {begin_count} begin / {end_count} end — skipping",
                target.display()
            );
            continue;
        }
        if begin_count > 1 || end_count > 1 {
            eprintln!(
                "gen-docs: ambiguous markers in {}: expected exactly one pair, \
                 found {begin_count} begin / {end_count} end — skipping",
                target.display()
            );
            continue;
        }

        let new_contents = marker_splice::splice_block(&current, MODELS_BEGIN, MODELS_END, &block);

        if new_contents == current {
            if !check_mode {
                println!("gen-docs: {} already up to date", target.display());
            }
            continue;
        }

        if check_mode {
            eprintln!(
                "gen-docs: {} is stale. Run `cargo run --bin gen-docs` to regenerate.",
                target.display()
            );
            eprintln!("--- expected block ---\n{block}\n--- end ---");
            return ExitCode::from(1);
        }

        if let Err(e) = marker_splice::write_atomic(target, &new_contents) {
            eprintln!("gen-docs: failed to write {}: {e}", target.display());
            return ExitCode::from(2);
        }
        println!("gen-docs: updated {}", target.display());
        any_updated = true;
    }

    if !any_updated && !check_mode {
        println!("gen-docs: all target files up to date");
    }

    ExitCode::SUCCESS
}

/// Walk up from CARGO_MANIFEST_DIR (or CWD as fallback) to find the repo root
/// (identified by `Cargo.toml` + `src/loop_engine/model.rs`).
fn repo_root() -> Result<PathBuf, String> {
    let start = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_dir().map_err(|e| e.to_string()))?;
    let mut cur = start.as_path();
    loop {
        if cur.join("Cargo.toml").is_file() && cur.join("src/loop_engine/model.rs").is_file() {
            return Ok(cur.to_path_buf());
        }
        cur = cur
            .parent()
            .ok_or_else(|| format!("walked past filesystem root from {}", start.display()))?;
    }
}

/// Extract model constants + effort table from model.rs and render the
/// markdown block (without the surrounding BEGIN/END markers).
fn render_block(model_rs: &Path) -> Result<String, String> {
    let source = fs::read_to_string(model_rs).map_err(|e| e.to_string())?;

    let model_re = Regex::new(r#"pub const (\w+): &str = "([^"]+)";"#).unwrap();
    let mut models: Vec<(String, String)> = model_re
        .captures_iter(&source)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .filter(|(name, _)| EXPECTED_MODEL_CONSTS.contains(&name.as_str()))
        .collect();
    // Preserve canonical order (Fable/frontier, Opus/standard, Sonnet, Haiku) regardless of file order.
    models.sort_by_key(|(name, _)| {
        EXPECTED_MODEL_CONSTS
            .iter()
            .position(|n| n == name)
            .unwrap_or(usize::MAX)
    });
    if models.len() != EXPECTED_MODEL_CONSTS.len() {
        return Err(format!(
            "expected {} model constants ({:?}), found {} ({:?})",
            EXPECTED_MODEL_CONSTS.len(),
            EXPECTED_MODEL_CONSTS,
            models.len(),
            models.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        ));
    }

    // Parse EFFORT_FOR_DIFFICULTY (Claude + Grok share it).
    let effort_re = Regex::new(
        r"pub const EFFORT_FOR_DIFFICULTY:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(?P<body>[^\]]+)\]",
    )
    .unwrap();
    let body = effort_re
        .captures(&source)
        .ok_or_else(|| "could not find EFFORT_FOR_DIFFICULTY constant".to_string())?
        .name("body")
        .unwrap()
        .as_str();
    // Row parser supports both "literal" values and bare IDENT (e.g. HAIKU_MODEL)
    // so tier tables can stay DRY by referencing the *_MODEL consts.
    let row_re =
        Regex::new(r#"\("([^"]+)",\s*(?:"([^"]*)"|"([^"]+)"|([A-Za-z_][A-Za-z0-9_]*))\)"#).unwrap();
    // Helper: given a row match, return (key, value) — value may come from quoted group or bare ident group.
    let extract_kv = |cap: &regex::Captures| -> (String, String) {
        let k = cap[1].to_string();
        // groups: 2=first-quoted, 3=second-quoted, 4=bare-ident
        let v = cap
            .get(2)
            .or_else(|| cap.get(3))
            .or_else(|| cap.get(4))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        (k, v)
    };
    let claude_grok_effort: Vec<(String, String)> =
        row_re.captures_iter(body).map(|c| extract_kv(&c)).collect();
    if claude_grok_effort.is_empty() {
        return Err("EFFORT_FOR_DIFFICULTY contains no rows".into());
    }

    // Parse CODEX_EFFORT_FOR_DIFFICULTY (capped at high by policy).
    let codex_effort_re = Regex::new(
        r"pub const CODEX_EFFORT_FOR_DIFFICULTY:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(?P<body>[^\]]+)\]",
    )
    .unwrap();
    let codex_body = codex_effort_re
        .captures(&source)
        .ok_or_else(|| "could not find CODEX_EFFORT_FOR_DIFFICULTY constant".to_string())?
        .name("body")
        .unwrap()
        .as_str();
    let codex_effort: Vec<(String, String)> = row_re
        .captures_iter(codex_body)
        .map(|c| extract_kv(&c))
        .collect();

    // Parse the declarative default tier tables (per-provider). Use a closure so it
    // can capture the outer `row_re` (a bare `fn` item cannot).
    let parse_tier_table = |const_name: &str| -> Result<Vec<(String, String)>, String> {
        let re = Regex::new(&format!(
            r"pub const {}:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(?P<body>[^\]]+)\]",
            const_name
        ))
        .unwrap();
        let body = re
            .captures(&source)
            .ok_or_else(|| format!("could not find {} constant", const_name))?
            .name("body")
            .unwrap()
            .as_str();
        Ok(row_re.captures_iter(body).map(|c| extract_kv(&c)).collect())
    };
    let mut claude_tiers = parse_tier_table("CLAUDE_DEFAULT_TIER_MODELS")?;
    let mut grok_tiers = parse_tier_table("GROK_DEFAULT_TIER_MODELS")?;
    let mut codex_tiers = parse_tier_table("CODEX_DEFAULT_TIER_MODELS")?;

    // Post-process: resolve any bare model-const identifiers (e.g. "HAIKU_MODEL")
    // to their actual ID strings using the already-parsed top-level model consts.
    // This lets the tier tables in model.rs stay DRY (ref the *_MODEL consts).
    let model_map: std::collections::HashMap<&str, &str> = models
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    for tiers in [&mut claude_tiers, &mut grok_tiers, &mut codex_tiers] {
        for (_k, v) in tiers.iter_mut() {
            if let Some(resolved) = model_map.get(v.as_str()) {
                *v = resolved.to_string();
            }
        }
    }

    let mut out = String::new();
    out.push_str("<!-- This block is auto-generated by `cargo run --bin gen-docs` from src/loop_engine/model.rs. Do not edit by hand. -->\n");
    out.push('\n');
    out.push_str("**Current model IDs** (bumped in `src/loop_engine/model.rs`):\n");
    out.push('\n');
    for (name, value) in &models {
        let tier = match name.as_str() {
            "FABLE_MODEL" => "Fable (frontier)",
            "OPUS_MODEL" => "Opus (standard)",
            "SONNET_MODEL" => "Sonnet (cost-efficient)",
            "HAIKU_MODEL" => "Haiku (cheapest)",
            _ => name.as_str(),
        };
        out.push_str(&format!("- **{tier}** → `{name}` = `{value}`\n"));
    }
    out.push('\n');
    out.push_str("**Difficulty → `--effort` mapping**:\n");
    out.push('\n');
    out.push_str("- Claude / Grok (`EFFORT_FOR_DIFFICULTY`): ");
    for (i, (d, e)) in claude_grok_effort.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("`{d}` → `{e}`"));
    }
    out.push('\n');
    out.push_str("- Codex (`CODEX_EFFORT_FOR_DIFFICULTY`, capped at `high` by policy): ");
    for (i, (d, e)) in codex_effort.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("`{d}` → `{e}`"));
    }
    out.push('\n');
    out.push('\n');

    // Tier matrix + anchor explanation (FR-010 / FR-001).
    out.push_str("**Capability tiers + anchor window (default `models` config)**:\n");
    out.push('\n');
    out.push_str(
        "Provider-neutral tiers (ordered Cheapest < CostEfficient < Standard < Frontier). ",
    );
    out.push_str(
        "The anchor (default `standard`) + difficulty offset produces the starting tier:\n",
    );
    out.push_str("- low difficulty → anchor − 1 (clamped at ladder bottom)\n");
    out.push_str("- medium → anchor\n");
    out.push_str("- high → anchor + 1 (clamped at ladder top)\n");
    out.push_str("See `anchored_tier` + `difficulty_offset` (single normalizer). ");
    out.push_str(
        "Sparse ladders: only defined rungs participate in clamp / escalate; gaps are skipped.\n",
    );
    out.push('\n');
    out.push_str("Default tier matrix (from the `_DEFAULT_TIER_MODELS` tables; empty = route with no model flag):\n");
    out.push('\n');
    // Simple markdown table. Columns: Tier | Claude | Grok | Codex
    out.push_str("| Tier | Claude | Grok | Codex |\n");
    out.push_str("|------|--------|------|-------|\n");
    // Use the tier order from ALL (cheapest first for table, or reverse for "high first").
    // Render in descending capability to match historical "frontier first" docs.
    let tier_order = ["frontier", "standard", "cost-efficient", "cheapest"];
    for t in tier_order {
        let claude = claude_tiers
            .iter()
            .find(|(k, _)| k == t)
            .map(|(_, v)| v.as_str())
            .unwrap_or("(n/a)");
        let grok = grok_tiers
            .iter()
            .find(|(k, _)| k == t)
            .map(|(_, v)| v.as_str())
            .unwrap_or("(n/a)");
        let codex = codex_tiers
            .iter()
            .find(|(k, _)| k == t)
            .map(|(_, v)| {
                if v.is_empty() {
                    "(no -m flag)"
                } else {
                    v.as_str()
                }
            })
            .unwrap_or("(n/a)");
        out.push_str(&format!("| {t} | {claude} | {grok} | {codex} |\n"));
    }
    out.push('\n');
    out.push_str("Codex routes are always explicit (`byIdPrefix` or `taskClasses` in `routing`); Codex is never inferred from a model string.\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_mgr::util::marker_splice;

    #[test]
    fn splice_replaces_between_markers() {
        let input = "head\n<!-- MODELS:BEGIN -->\nOLD\n<!-- MODELS:END -->\ntail\n";
        let out = marker_splice::splice_block(input, MODELS_BEGIN, MODELS_END, "NEW\n");
        assert!(out.contains("NEW"));
        assert!(!out.contains("OLD"));
        assert!(out.contains("head"));
        assert!(out.contains("tail"));
    }

    #[test]
    fn splice_is_idempotent() {
        let input = "head\n<!-- MODELS:BEGIN -->\nBLOCK\n<!-- MODELS:END -->\ntail\n";
        let once = marker_splice::splice_block(input, MODELS_BEGIN, MODELS_END, "BLOCK\n");
        let twice = marker_splice::splice_block(&once, MODELS_BEGIN, MODELS_END, "BLOCK\n");
        assert_eq!(once, twice);
    }
}
