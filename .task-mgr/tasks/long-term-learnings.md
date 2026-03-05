# Long-Term Learnings

## Large File Refactoring

### Extraction Threshold Guidance

**When to extract:**
- File has 3+ genuinely distinct responsibilities (not just "query + format" which is one concern)
- Functions are reusable outside the original module (e.g., dependency checking used by both completion and next-task selection)
- External callers already exist or are clearly needed
- Production code exceeds ~400 lines AND has separable concerns

**When to skip (EXPLICIT_SKIP):**
- File size is driven by tests, not production code (oauth.rs: 197L prod / 660L tests = SKIP)
- Functions are tightly coupled orchestration steps (splitting scatters the flow)
- Single function with complex internals but clear single responsibility
- Flat idiomatic patterns (e.g., Rust match with 34 clean arms — don't split into dispatch helpers)
- Structural files expected to be large (error.rs with error enum + constructors is idiomatic Rust)
- Extraction would produce two modules under ~100 lines each — marginal benefit

**Threshold rule of thumb:** If the extraction target is under ~70 lines of production code, it's probably not worth a new module unless it has clear external callers.

### Visibility Patterns

- Extracted private functions become `pub(crate)` — narrowest visibility that works
- Public functions stay `pub` in the new module
- Re-export via `pub use` in the original module for backward compatibility (even if no current callers use the old path — prevents future breakage)
- Constants stay in the original file; new module imports them (avoids breaking other consumers)

### Test Movement Patterns

- Tests follow their functions to the new module
- Duplicate tests (testing the same function from both old and new locations) must be removed
- Test helpers that are module-specific stay in that module; shared helpers go to `test_utils`
- Always verify test count before and after — net change should be zero (or positive if adding new tests for extracted code)
- `#[cfg(test)]` inline modules are needed for testing private functions

### Annotation Preservation

- `#[allow(clippy::...)]` annotations must travel with the code they suppress
- `#[cfg(unix)]` and `#[cfg(not(unix))]` paired blocks must stay together
- Line-level annotations (not module-level) are easy to miss during extraction

### Extraction Protocol

1. **Record baseline** (`cargo test | grep 'test result'`) before any changes
2. **Map call graph** — grep for `module::function` across all of `src/`
3. **Create new file** with `//!` doc comment explaining the module's purpose
4. **Move functions**, adjust visibility, move associated tests
5. **Update mod.rs** (keep declarations alphabetical), add imports, update external callers
6. **Clean up** unused imports in both source and destination files
7. **Verify**: `cargo build`, `cargo test` (match baseline), `cargo clippy -- -D warnings`, `cargo fmt --check`

### PRD Assumptions vs Reality

| PRD Assumption | Reality |
|---|---|
| oauth.rs needs browser/PKCE/callback extraction | Those features don't exist — oauth.rs is 197L prod code |
| main.rs has centralized DB setup to extract | `open_connection` is called per-match-arm, no centralized block |
| claude.rs is ~1181 lines | Actually 2272 lines (stream-json feature added during dev) |
| archive.rs and ingestion extract_learnings are duplicates | Different inputs/outputs/purposes — no dedup needed |
| Most Tier 4 files need extraction | 14/17 are COHESIVE, only 2 warranted extraction |
| engine.rs target <2500L after P1 | Achieved 2546L — close but not exact, targets were miscalibrated |

### Phase-by-Phase Summary

| Phase | Tier | Files | Extractions | Skips | Line Reduction |
|---|---|---|---|---|---|
| P1 | 1 (2600-4500L) | 3 files | 9 new modules | 0 | engine.rs 4544→2546, prompt.rs 3753→2990, env.rs 2688→1631 |
| P2 | 2 (1000-2300L) | 5 files | 4 new modules | 1 (archive.rs) | claude.rs: watchdog+status_queries+status_display extracted; calibrate.rs: calibrate_math extracted |
| P3 | 3 (780-860L) | 3 files | 1 new module | 2 (oauth.rs, main.rs) | signals.rs: guidance.rs extracted (~70L prod) |
| P4 | 4 (596-912L) | 17 files + claude-loop.sh | 1 new module | 15 (14 COHESIVE + 1 MONITOR) | complete.rs: dependency_checker.rs extracted |

**Total: 15 new modules extracted, 18 files skipped/cohesive**

### Key Patterns

1. **SKIP rate increases as file size decreases**: P1: 0/3, P2: 1/5, P3: 2/3, P4: 15/17. Smaller files are more likely to be cohesive already.
2. **Test count is the anchor**: Every extraction must preserve exact test count. Recording baseline before starting is non-negotiable.
3. **Assessment before extraction**: Reading every file before judging prevents waste. Line count alone is not a signal — prod/test split matters.
4. **Diminishing returns are real**: P4's single extraction (dependency_checker.rs) was the only genuine multi-responsibility file among 17 candidates.
5. **Re-export preserves backward compatibility**: `pub use new_module::function` in the original file means zero caller changes needed (though updating callers to import directly is cleaner).
6. **Batch removals shift line numbers**: When removing multiple blocks from one file, work bottom-to-top or re-read between removals.
