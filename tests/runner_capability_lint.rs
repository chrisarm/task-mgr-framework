//! Grep lint: capability-driven fields must not be silently discarded.
//!
//! Once `dispatch()` enforces capabilities before spawning, any `RunnerOpts`
//! field that maps to a `RunnerCapability` variant will never be set to a
//! non-default value on a runner that rejects it.  A `<field>: _,` destructure
//! inside a `spawn` impl therefore means the field is accepted but silently
//! ignored — a bug.  This test catches regressions at `cargo test` time
//! without requiring an AST parser.
//!
//! The list of capability field names is kept in sync with the `CHECKS`
//! registry table in `src/loop_engine/runner.rs`.

use std::fs;
use std::path::Path;

/// Field names that appear in the `CHECKS` registry table.
/// Update this list whenever a new `RunnerCapability` variant is added.
const CAPABILITY_FIELDS: &[&str] = &[
    "use_pty",
    "stream_json",
    "effort",
    "disallowed_tools",
    "cleanup_title_artifact",
];

#[test]
fn no_capability_field_silently_discarded_in_spawn_impls() {
    let runner_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/loop_engine/runner.rs");

    let src = fs::read_to_string(&runner_src)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", runner_src.display()));

    let mut violations: Vec<String> = Vec::new();

    for (line_no, line) in src.lines().enumerate() {
        for field in CAPABILITY_FIELDS {
            // Pattern: `<field>: _` followed by optional whitespace and a comma
            // (i.e., the field is present in a destructure but its value is dropped).
            let pattern = format!("{field}: _");
            if line.contains(&pattern) {
                violations.push(format!(
                    "{}:{} — `{field}: _` silently discards a capability field \
                     (capabilities are enforced at dispatch; remove the field from \
                     the destructure or use `..` to elide unused fields)",
                    runner_src.display(),
                    line_no + 1,
                ));
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Capability-field silent-discard lint failed ({} violation{}):\n{}",
            violations.len(),
            if violations.len() == 1 { "" } else { "s" },
            violations.join("\n"),
        );
    }
}
