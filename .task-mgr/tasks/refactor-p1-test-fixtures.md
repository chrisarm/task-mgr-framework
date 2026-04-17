# PRD: Refactor P1 — Shared `TestDb` Test Fixture

**Type**: Refactor
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-04-15
**Status**: Draft

---

## 1. Overview

### Problem Statement

~40 test modules across the crate hand-roll the same 4-line database setup block:

```rust
let temp_dir = TempDir::new().unwrap();
let mut conn = open_connection(temp_dir.path()).unwrap();
create_schema(&conn).unwrap();
run_migrations(&mut conn).unwrap();
```

There are already **two parallel helpers** that partially consolidate this pattern —
`src/learnings/test_helpers.rs::setup_db()` and `src/loop_engine/test_utils.rs::setup_test_db()` —
but they're module-scoped and have drifted (identical body, different return-pattern
expectations at call sites). The rest of the crate still reproduces the raw 4-liner.
The result is:

1. **Drift risk.** Two helpers exist today; when schema setup changes (e.g., a new
   migration or a pragma requirement) any of ~40 sites can silently diverge.
2. **Awkward ergonomics.** The `(TempDir, Connection)` tuple shape forces every test
   to bind an unused `_tmp` local to keep the directory alive; callers that want the
   path write `temp_dir.path()` repeatedly.
3. **No crate-wide contract.** Integration between modules (e.g., a test that exercises
   learnings through a loop-engine entrypoint) can't pick a single blessed helper —
   both live under `pub(crate)` visibility scoped to their own tree.

### Background

- Learning #55 (`Clean test patterns: pure functions and single shared setup`, confidence
  high, 10/19 applied) explicitly endorses "single shared setup" as the pattern to
  follow in this codebase.
- `curate-learnings-p1/p2/p3` established the phased-PRD pattern used here.
- This PRD is **Phase 1 of the approved 4-phase plan** at
  `$HOME/.claude/plans/drifting-soaring-ocean.md`. Phases 2–4 are explicitly
  out of scope.

---

## 2. Goals

### Primary Goals

- [ ] Introduce a single `TestDb` struct + `setup_test_db()` helper in `src/db/test_utils.rs`
      that returns a richer shape than `(TempDir, Connection)` and owns the tempdir for
      scope-based cleanup.
- [ ] Replace every duplicated `TempDir + open_connection + create_schema + run_migrations`
      block across `src/**/*.rs` tests with the new helper.
- [ ] Delete `src/learnings/test_helpers.rs::setup_db()` and
      `src/loop_engine/test_utils.rs::setup_test_db()`; preserve the other helpers those
      files export (e.g., `EnvGuard`, `CLAUDE_BINARY_MUTEX`, `insert_test_learning`,
      `setup_git_repo`).

### Success Metrics

- **Duplication**: `rg -U 'TempDir::new\(\).*\n.*open_connection.*\n.*create_schema' src/` returns zero non-helper hits.
- **Behavior**: `cargo test` passes with identical test counts before and after (no
  disabled or weakened tests).
- **Lint cleanliness**: `cargo clippy -- -D warnings` green on every intermediate commit.
- **LOC reduction**: ≥120 lines of duplicated test scaffolding removed (4 lines × ≥30
  sites, conservative estimate; actual is likely higher).

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Byte-identical schema state.** `TestDb` must produce a database state functionally
  identical to the existing 4-line pattern — same pragmas, same schema, same migrations
  applied. Any test that depended on pragma or migration order must continue to pass.
- **TempDir lifetime.** The `TempDir` MUST outlive every use of the `Connection`.
  Dropping `TestDb` before finishing queries must not be possible without a compile
  error. Pattern: `TestDb` owns both fields; `&TestDb` methods borrow the connection
  for the duration of the call.
- **Deterministic cleanup.** No leaked `/tmp/.tmpXXXX` directories after a green test run.

### Performance Requirements

- **Best effort.** Tests are already fast (~1ms per DB setup). No new slow paths; do
  not add logging, sleeps, or retries to the helper.
- **No fixture caching across tests.** Each test gets its own TempDir + connection —
  attempting to pool is a well-known antipattern for SQLite-backed tests because of
  WAL and file-lock semantics.

### Style Requirements

- **No `.unwrap()` in the helper body.** The helper is called by ~40 tests; an assertion
  failure should produce a message that points to `test_utils.rs`, not a raw panic. Use
  `.expect("test DB setup: <reason>")` with distinct messages per step. Call sites may
  continue to use tuple destructuring.
- **`pub(crate)`, not `pub`.** The helper is an internal testing tool. Behind
  `#[cfg(test)]` in `src/db/mod.rs` so it contributes zero bytes to the release binary.
- **No feature flag.** No `test-utils` Cargo feature is needed — integration tests
  (under a top-level `tests/` directory) were surveyed and don't consume these helpers.
- **Follow existing codebase patterns** for the remainder: error types, imports,
  `#[cfg(test)]` module gating.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|----------------|-------------------|
| Migration-stepping tests (`src/db/migrations/tests.rs`, `v12..v16.rs`) that call `migrate_up` explicitly and need an un-migrated DB | A one-size-fits-all "fully migrated" helper would force these tests to undo migrations | Provide a second helper `setup_test_db_unmigrated()` returning a `TestDb` with schema but no migrations applied, or document the explicit primitive pattern and exempt these sites |
| Tests that take `&mut Connection` for write-heavy paths | `TestDb` owns the connection; returning `&Connection` forces callers to mutate through `RefCell` or similar | `TestDb.conn` is `pub` and moved out when needed, OR expose `conn()` / `conn_mut()` methods — see Approaches section |
| Tests that spawn child processes needing the DB path (e.g., `task-mgr` subprocess tests) | `.db_dir()` must return a `&Path` that survives across `process::Command` invocations | `TestDb.db_dir() -> &Path` delegating to `self.tmp.path()` |
| Tests that currently use `.expect("msg")` instead of `.unwrap()` for diagnostic purposes | Wholesale mechanical replacement could lose diagnostic context | Keep the existing expectation message pattern intact on the helper call site where the test author clearly chose one |
| Tests that open a **second** connection to the same tempdir (e.g., concurrent-access tests) | Need access to the tempdir path to open their own connection | `db.db_dir()` lets them do `open_connection(db.db_dir())` |
| Dropping `TestDb` before final assertions | Would delete the tempdir and leave the connection pointing at a removed file | Compile-time scope enforces this: binding `let db = setup_test_db();` keeps both alive to end-of-scope |

---

## 3. User Stories

### US-001: Single Canonical Test Fixture

**As a** contributor writing a new test that needs a task-mgr database
**I want** one obvious import (`use crate::db::test_utils::setup_test_db;`) that gives
me a ready-to-use `TestDb`
**So that** I don't have to look up the four-line setup incantation or wonder which
of the two existing helpers is "the right one"

**Acceptance Criteria:**

- [ ] New test author can copy a single line (`let db = setup_test_db();`) and start
      writing assertions against `&db.conn`.
- [ ] `TestDb` exposes `.conn` (pub field) and `.db_dir() -> &Path`. Optional
      convenience: `.conn_mut()` if lint patterns demand mutable access across
      boundaries.

### US-002: Zero Duplication Sweep

**As a** reviewer of this refactor PRD
**I want** every existing call site using the 4-line pattern migrated to `setup_test_db()`
**So that** a future schema/migration change flows through exactly one choke point

**Acceptance Criteria:**

- [ ] `rg -U 'TempDir::new\(\).*\n.*open_connection.*\n.*create_schema' src/` returns
      zero results outside `src/db/test_utils.rs`.
- [ ] `src/learnings/test_helpers.rs::setup_db()` and
      `src/loop_engine/test_utils.rs::setup_test_db()` are deleted; their callers
      point at the new helper.

### US-003: Migration Tests Keep Working

**As a** maintainer of `src/db/migrations/`
**I want** tests that explicitly call `migrate_up` / `migrate_down` to continue
functioning after the refactor
**So that** migration coverage is preserved

**Acceptance Criteria:**

- [ ] Every pre-existing migration test passes unchanged OR uses a documented
      alternative helper (`setup_test_db_unmigrated()`).
- [ ] No test is silently converted from "partial migration" to "full migration" (would
      mask version-specific bugs).

---

## 4. Functional Requirements

### FR-001: `TestDb` Struct and `setup_test_db()` Entry Point

**Description**: Introduce a new module at `src/db/test_utils.rs` that defines:

```rust
use rusqlite::Connection;
use std::path::Path;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};

pub struct TestDb {
    pub conn: Connection,
    pub tmp: TempDir,
}

impl TestDb {
    pub fn db_dir(&self) -> &Path {
        self.tmp.path()
    }
}

pub fn setup_test_db() -> TestDb {
    let tmp = TempDir::new().expect("test DB setup: create tempdir");
    let mut conn = open_connection(tmp.path()).expect("test DB setup: open connection");
    create_schema(&conn).expect("test DB setup: create schema");
    run_migrations(&mut conn).expect("test DB setup: run migrations");
    TestDb { conn, tmp }
}

pub fn setup_test_db_unmigrated() -> TestDb {
    let tmp = TempDir::new().expect("test DB setup: create tempdir");
    let conn = open_connection(tmp.path()).expect("test DB setup: open connection");
    create_schema(&conn).expect("test DB setup: create schema");
    TestDb { conn, tmp }
}
```

**Details:**

- Fields are `pub` (not behind `pub(crate)`) within the `#[cfg(test)]` module — tests
  in any module can write `&db.conn`, `&mut db.conn`, `db.db_dir()`.
- Both helpers return `TestDb` so test code can move seamlessly between them if scope
  changes.
- `src/db/mod.rs` adds `#[cfg(test)] pub(crate) mod test_utils;`.

**Validation:**

- New unit test in `src/db/test_utils.rs` itself: `setup_test_db()` produces a DB at
  schema version `CURRENT_SCHEMA_VERSION`; `setup_test_db_unmigrated()` produces a DB
  at a schema version < `CURRENT_SCHEMA_VERSION` when any migrations exist.

### FR-002: Delete Both Legacy Helpers

**Description**: Remove `src/learnings/test_helpers.rs::setup_db()` (lines 9-15) and
`src/loop_engine/test_utils.rs::setup_test_db()` (lines 69-75). Do NOT touch any other
item those files export.

**Details:**

- `src/learnings/test_helpers.rs` also defines `retire_learning` and
  `insert_task_with_files` — **keep these**. File may remain but shrink.
- `src/loop_engine/test_utils.rs` also defines `CLAUDE_BINARY_MUTEX`, `EnvGuard`,
  `insert_test_learning`, `setup_git_repo`, `get_task_status`, `insert_task`,
  `insert_relationship`, `insert_prd_metadata`, `insert_done_task`, and similar —
  **keep all of these**. Only `setup_test_db` moves.

**Validation:**

- `rg 'fn setup_db\(\)|fn setup_test_db\(\)' src/ --glob '!src/db/test_utils.rs'` returns zero hits.
- All helpers listed above as "keep" are still exported and still used.

### FR-003: Migrate All Call Sites

**Description**: Every file that reproduces the 4-line pattern is updated to call
`setup_test_db()`. Call-site author-style (e.g., `.expect("create temp dir")`) is
not preserved — the helper's `.expect()` messages supersede.

**Details (preliminary site list — final sweep is part of task execution):**

Tests under `src/commands/`:
`apply_learning.rs`, `complete.rs`, `curate/tests.rs`, `decisions.rs`,
`dependency_checker.rs`, `doctor/tests.rs`, `export/tests.rs`, `fail/tests.rs`,
`history.rs`, `import_learnings/{mod.rs,tests.rs}`, `init/{import.rs,tests.rs}`,
`irrelevant.rs`, `learnings.rs`, `list.rs`, `migrate.rs`, `next/tests.rs`, `recall.rs`,
`reset.rs`, `review.rs`, `run.rs`, `show.rs`, `skip.rs`, `stats.rs`, `unblock.rs`,
`worktrees.rs`.

Tests under `src/learnings/`:
`bandit.rs`, `crud/tests.rs`, `embeddings/mod.rs`, `recall/tests.rs`,
`retrieval/tests.rs`.

Tests under `src/db/`:
`connection.rs`, `lock.rs`, `migrations/{mod.rs,tests.rs,v12.rs,v13.rs,v14.rs,v15.rs,v16.rs}`,
`schema/{tests.rs,key_decisions.rs}`, `soft_archive.rs`.

Tests under `src/loop_engine/`: this is the heaviest group — many tests already use
the old `setup_test_db`. Every `use crate::loop_engine::test_utils::setup_test_db;`
becomes `use crate::db::test_utils::setup_test_db;`.

**Migration tests exception**: Any test in `src/db/migrations/**` that depends on
running migrations incrementally (observed in v13.rs, tests.rs) is migrated to
`setup_test_db_unmigrated()` plus explicit `migrate_up(&mut conn).unwrap()` calls as
today.

**Call-site shape change**: from `let (tmp, conn) = setup_test_db();` to
`let db = setup_test_db();` with `&db.conn` / `&mut db.conn` / `db.db_dir()` at use
sites. The existing `(TempDir, Connection)` destructuring is removed across all call sites.

**Validation:**

- `cargo test` passes with zero test-count regression.
- Final grep returns zero duplication (see Success Metrics).

---

## 5. Non-Goals (Out of Scope)

- **Phases 2-4 of the plan** (`warn()/error()` helpers, `PromptBuilder`, `run_loop`
  decomposition) — each has its own PRD.
- **Changing `open_connection` / `create_schema` / `run_migrations` behavior** — these
  functions are used both in production and tests; this refactor only changes where
  they're called from.
- **Introducing a Cargo `test-utils` feature flag** — no integration-test consumers
  exist; `#[cfg(test)]` suffices.
- **Parameterizing `setup_test_db` with Schema overrides or fixtures data** — the
  helper stays minimal. Tests that need preloaded tasks/learnings continue to call
  existing helpers (`insert_task`, `insert_test_learning`, etc.) after the DB is up.
- **Snapshot / golden-file testing infrastructure** — possible follow-up, not this PRD.
- **Touching `#[cfg(feature = "integration-tests")]` or any CI/Cargo.toml** — the
  refactor is source-only.

---

## 6. Technical Considerations

### Affected Components

- `src/db/test_utils.rs` — **new file**. `TestDb` struct + `setup_test_db()` +
  `setup_test_db_unmigrated()` + unit test.
- `src/db/mod.rs` — **modify**. Add `#[cfg(test)] pub(crate) mod test_utils;`.
- `src/learnings/test_helpers.rs` — **modify**. Delete `setup_db()`; retain
  `retire_learning`, `insert_task_with_files`. Callers (`src/learnings/crud/tests.rs`,
  `src/learnings/recall/tests.rs`, `src/learnings/retrieval/tests.rs`,
  `src/learnings/bandit.rs`) move to new helper.
- `src/loop_engine/test_utils.rs` — **modify**. Delete `setup_test_db()` (lines 69-75).
  Retain every other symbol. Update internal call sites within `loop_engine/*` to
  import from `crate::db::test_utils`.
- `src/commands/**/*.rs` (tests), `src/db/migrations/**/*.rs` (tests),
  `src/db/schema/**/*.rs` (tests), `src/db/connection.rs` (tests), `src/db/lock.rs`
  (tests), `src/db/soft_archive.rs` (tests), `src/learnings/embeddings/mod.rs` (tests) —
  **mechanically updated** to use new helper.

### Dependencies

- `tempfile` (already in `[dev-dependencies]`): used for `TempDir`.
- `rusqlite` (already in direct deps): used for `Connection`.
- No new Cargo deps. No feature flags.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **A1:** Tuple `(TempDir, Connection)` — promote `loop_engine::test_utils::setup_test_db` verbatim to `db::test_utils` | Tiniest diff. Matches existing learnings/ and loop_engine/ shape. Zero call-site changes in loop_engine/. | Keeps the awkward `_tmp` binding at call sites. Doesn't address the plan's explicit goal of "eliminating the awkward `_tmp` binding many call sites use today" (user's choice). | Rejected |
| **A2: `TestDb` struct with pub fields** | Owns both TempDir + Connection by scope. Rich methods (`.db_dir()`, optional `.conn_mut()`). Eliminates `_tmp` binding. Zero ergonomics cost for callers. | Call sites move from `(tmp, conn) = ...` → `let db = ...` — a larger mechanical diff. Every migrated test re-binds. | **Preferred** (matches user's selection) |
| **A3:** Feature-gated `pub` module for integration tests | Would let `tests/*.rs` integration tests reuse fixture. | No integration tests currently consume these helpers. Adds complexity for zero benefit. | Rejected |

**Selected Approach**: **A2**. The user explicitly chose the `TestDb` struct over the
tuple during question-gathering. The additional mechanical churn (rebinding at every
call site) is justified by the ergonomic win — unused `_tmp` bindings disappear,
`.db_dir()` replaces `temp_dir.path()`, and the single-type shape opens the door to
future enrichment (e.g., adding `runs_count()`, `task()`, etc. as helpers on `TestDb`
in later phases without breaking callers).

**Phase 2 Foundation Check**: `TestDb` as a struct lays groundwork for future
testing-DX improvements (e.g., a `TestDb::with_task(...)` builder, a `TestDb::seed()`
hook). The tuple form would require a breaking change to adopt any of those. Cost now:
~1 hour of mechanical diff across ~40 files. Benefit avoided: rewriting every call
site a second time. 1:10 ratio easily clears.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| **R1**: A migrated test silently weakens because the new helper subtly differs from the old (e.g., different pragma order, different `run_migrations` parameterization) | Medium — could mask a real bug | Low — helper body is literally the same 4 calls | Compare bytes of `sqlite3 .db .schema` output from old vs new helper on a representative test; diff both before/after migration of one test file as the first verification step |
| **R2**: A test depends on the `_tmp` binding being named exactly `temp_dir` (e.g., passes `temp_dir.path()` to a subprocess) and breaks when rebound | Medium — test compile/runtime failure | Medium — some tests do pass `.path()` explicitly | Search for `temp_dir.path()` / `tmp.path()` on tests before migration; in those cases replace with `db.db_dir()` |
| **R3**: A migration test that calls `migrate_up` explicitly gets converted to the fully-migrated helper, silently removing version-specific coverage | High — loses migration test coverage | Medium — ~6 migration files have this pattern | Review every `src/db/migrations/**/*.rs` test module individually; use `setup_test_db_unmigrated()` for those; add a reviewer-enforced task explicitly for this subset |
| **R4**: Dropped test isolation (e.g., sharing a TempDir accidentally across tests) | High — nondeterministic test failures | Low — `TestDb` is scoped per-function | Every `setup_test_db()` call produces a fresh `TempDir`; no shared state; covered by existing `cargo test` parallelism |

### Security Considerations

- `TempDir::new()` uses OS temp directory (`/tmp` on Linux). No secrets written to it
  by tests. No change in security posture.
- No change to `open_connection` pragmas or lock semantics.

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
|-----------------|-----------|-------------------|-----------------|--------------|
| `db::test_utils::setup_test_db` | `fn setup_test_db() -> TestDb` | `TestDb { conn: Connection, tmp: TempDir }` with all migrations applied | Panics via `.expect(...)` on tempdir / open / schema / migration failure | Creates a TempDir, opens SQLite file, writes schema + migrations |
| `db::test_utils::setup_test_db_unmigrated` | `fn setup_test_db_unmigrated() -> TestDb` | `TestDb` with schema only, no migrations applied | Panics via `.expect(...)` on tempdir / open / schema failure | Creates TempDir, opens SQLite file, writes schema |
| `db::test_utils::TestDb::db_dir` | `fn db_dir(&self) -> &Path` | Path to the tempdir | Infallible | None |

All items are `#[cfg(test)]`-gated and `pub(crate)`; they do NOT contribute to the
release binary.

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|-----------------|-------------------|--------------------|-----------|-----------|
| `learnings::test_helpers::setup_db` | `fn setup_db() -> (TempDir, Connection)` | *(deleted)* | Yes — test-only | Replace all call sites with `db::test_utils::setup_test_db()` |
| `loop_engine::test_utils::setup_test_db` | `fn setup_test_db() -> (TempDir, Connection)` | *(deleted)* | Yes — test-only | Replace all call sites with `db::test_utils::setup_test_db()` |

No production/public-API surface changes.

### Data Flow Contracts

**N/A** — this refactor adds no cross-module data access. `TestDb` is a leaf struct
used in test setup; it doesn't participate in any pipeline that crosses module
boundaries at runtime.

### Consumers of Changed Behavior

`setup_db()` and the existing `loop_engine::test_utils::setup_test_db()` are consumed
only by tests. The full consumer list is every file listed in FR-003. No production
code paths depend on either helper.

| File:Line | Usage | Impact | Mitigation |
|-----------|-------|--------|------------|
| `src/learnings/crud/tests.rs`, `src/learnings/recall/tests.rs`, `src/learnings/retrieval/tests.rs`, `src/learnings/bandit.rs` | Call `learnings::test_helpers::setup_db()` | NEEDS MIGRATION | Rewrite imports + call sites to `db::test_utils::setup_test_db()` |
| `src/loop_engine/{env,worktree,status,status_queries,feedback,calibrate,prompt,engine,prd_reconcile,...}.rs` (see Grep output above) | Call `loop_engine::test_utils::setup_test_db()` | NEEDS MIGRATION | Rewrite imports to `crate::db::test_utils::setup_test_db`. Note: many call sites destructure `(tmp, conn)` — these must rebind to `let db = setup_test_db();` |
| All ~30 `src/commands/**/*.rs` tests reproducing the 4-liner | Hand-rolled setup | NEEDS MIGRATION | Mechanical replacement |
| All `src/db/migrations/**` tests that reproduce the 4-liner | Hand-rolled setup | NEEDS MIGRATION (with care for partial-migration tests) | Use `setup_test_db()` where full migration is wanted; `setup_test_db_unmigrated()` for version-stepping tests |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
|-----------|---------|------------------|----------------------|
| 4-line setup in non-migration tests | Tests that want a fully-migrated DB | Hand-written | Call `setup_test_db()` |
| 4-line setup in migration tests | Tests that want to run migrations stepwise | Hand-written (then explicit `migrate_up` calls, sometimes) | Call `setup_test_db_unmigrated()` if stepping; `setup_test_db()` if only the final state is asserted |

### Inversion Checklist

- [x] All callers identified — grep in Step 4 above enumerates 40+ files.
- [x] Routing/branching decisions — n/a (no runtime branching depends on these helpers).
- [x] Tests that validate current behavior — all existing tests must pass; this is the
      primary acceptance criterion.
- [x] Different semantic contexts for the same pattern — migration tests vs. non-migration
      tests are the one semantic distinction, captured by the two-helper split.

### Documentation

| Doc | Action | Description |
|-----|--------|-------------|
| `docs/ARCHITECTURE.md` | *(possibly)* update | Add a one-line pointer to `src/db/test_utils.rs` in any "Testing" section, if one exists. If not, skip. |
| `CLAUDE.md` (project-level) | *(optional)* update | Add a short "Testing Database Setup" note so future agents default to the helper. Low priority — if the module has good rustdoc it's self-documenting. |

No other doc updates required. This is test-only plumbing; architecture docs and runbooks
are unchanged.

---

## 7. Open Questions

- [ ] Should `TestDb` expose `conn_mut()` / `conn()` methods, or is the `pub conn:
      Connection` field sufficient? **Default: pub field only** — rusqlite's
      `Connection` methods take `&self` for queries and `&mut self` for transactions,
      which Rust's borrow checker already handles through the struct's field borrow.
      Revisit if the migration sweep surfaces awkward patterns.
- [ ] Should `src/learnings/test_helpers.rs` be deleted entirely (its remaining helpers
      `retire_learning` and `insert_task_with_files` move to `db/test_utils.rs` or a
      `learnings/fixtures.rs`)? **Default: keep the file, just remove `setup_db()`.**
      Cross-cutting helper organization is out of scope for P1.
- [ ] Should the two-helper split (`setup_test_db` vs. `setup_test_db_unmigrated`) be
      one function with a param (`setup_test_db(migrate: bool)`) instead?
      **Default: keep them separate.** Named helpers are more discoverable than a
      bool-flag; it's three extra lines of helper code for a real ergonomic win.
- [ ] Should this PRD explicitly call out that `tests/*.rs` integration tests (if any
      are added later) should consume the helper via a `test-utils` feature flag?
      **Default: no.** Document-by-example when the need arises.

---

## Appendix

### Related Documents

- `$HOME/.claude/plans/drifting-soaring-ocean.md` — approved 4-phase refactor plan.
- `src/learnings/test_helpers.rs` — existing helper (to be deleted).
- `src/loop_engine/test_utils.rs` — existing helper (to be deleted, other symbols kept).
- `curate-learnings-p1/p2/p3` PRDs in `.task-mgr/tasks/` — precedent for phased refactors.

### Glossary

- **`TestDb`**: the new struct bundling `TempDir` + `Connection` for test fixtures.
- **`setup_test_db()`**: the canonical fully-migrated test DB entry point.
- **`setup_test_db_unmigrated()`**: variant that skips `run_migrations` for
  version-stepping migration tests.
- **"the 4-liner"**: shorthand for `TempDir::new() + open_connection + create_schema +
  run_migrations` (sometimes with `.expect(msg)` variants).
