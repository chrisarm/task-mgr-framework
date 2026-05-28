# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Logging Standardization** for **task-mgr**.

## Problem Statement

task-mgr has **no logging framework** — ~511 `eprintln!` + many `println!` calls, no `tracing`/`log` crate, no subscriber. A `/spike` (2026-05-27, see `tasks/logging-standardization-spike.md`) established that this is **not** a file-descriptor routing problem: product UX (`emit_prefixed_lines` → stderr), byte-locked operator-contract messages (`lifecycle_stderr_contract.rs`, operators grep them), and internal diagnostics **all share stderr today**. The boundary is a **classification rule**.

This effort introduces a **`ui::` product channel** (preserve exact bytes + audience FD, never decorate) for human-facing output and byte-locked operator contracts; a **`tracing` subscriber + rolling file appender** for internal diagnostics (console WARN+ via `TASK_MGR_LOG`, DEBUG+ to `.task-mgr/logs/task-mgr-<prefix>.log`); and a **per-slot-per-iteration capture file** for child-agent (grok) raw stderr. **Full-repo migration.** Log files are **prefix-suffixed per task-list**, exactly like `tasks/progress-<prefix>.txt`.

---

## Logging Boundary Contract (apply EVERY iteration — this is CONTRACT-LOG-001)

**Four channels. The first two stay OUT of `tracing`:**

| # | Channel | What | FD | Route to |
|---|---------|------|----|----------|
| **A** | Product-UX | slot-prefixed agent text, iteration banners, progress headers | stderr | `ui::emit` / `ui::emit_prefixed` |
| **A** | Product-DATA | `list`/`show`/`stats`/`export` machine-readable output (scripts/pipes consume it) | **stdout** | `ui::emit_data` |
| **A2** | Operator-contract | byte-locked/grepped messages (PRD-sync warning, overflow announcements, escape-valve line) | stderr, **byte-exact** | `ui::emit` (NEVER tracing) |
| **B** | Diagnostics | debug/trace/info nobody snapshot-tests or watches | — | `tracing::{trace,debug,info,warn,error}!` |
| **C** | Child raw stderr | grok internal tracing / 502 HTML / telemetry export errors | — | per-slot-per-iter file (FEAT-006) |

**Discriminator decision tree** — for every `eprintln!`/`println!` you migrate:

```
Is the exact byte text asserted by a test, documented for operators to grep,
or a deliberate human-facing banner/summary/CLI-data line?
├─ YES → channel A / A2.  Route through ui::emit / ui::emit_data / ui::emit_prefixed.
│        Preserve bytes + FD.  NEVER add level/timestamp.  NEVER move to tracing.
│        (CLI data that a script could parse → ui::emit_data on STDOUT.)
└─ NO  → channel B.  Internal diagnostic → tracing::{debug,warn,...}!
         Default level = its honest noise floor (most "FYI" lines → debug!).
```

**Ambiguous?** Default to `ui::emit` (preserve visibility) and note it in the progress log for REVIEW-001. For a line a script might consume, default to `ui::emit_data` (stdout).

**The single most dangerous mistake:** a mechanical `s/eprintln!/tracing::warn!/` — it (a) prepends `WARN task_mgr::...` + a timestamp to byte-locked A2 lines → breaks `lifecycle_stderr_contract.rs` + operator greps, and (b) buries product banners below the WARN+ console default into a file nobody watches. Classify, don't substitute.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry, decide handling before coding.
3. **Classify before editing** — for a migration task, walk each call site through the decision tree above; don't batch-replace.
4. **Pick an approach** — Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration).

---

## Priority Philosophy

In order: **PLAN** (classify call sites first) → **PHASE 2 FOUNDATION** (the `ui::`/tracing boundary is what 6 migrations build on — get it right once) → **FUNCTIONING CODE** (output still appears where operators expect) → **CORRECTNESS** (byte-locked contracts stay byte-identical; scoped tests pass) → **CODE QUALITY** (no raw prints left in migrated modules) → **POLISH** (docs).

Non-negotiables: satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `high`/`modifiesBehavior` tasks, note the one alternative you rejected.

**Prohibited outcomes:**

- Mechanical `s/eprintln!/tracing::warn!/` that decorates byte-locked operator lines with a WARN/timestamp prefix
- Moving a human-facing banner or CLI data line into a DEBUG log file nobody watches
- A `tracing` init that aborts the CLI when the log file can't be opened (must degrade to console-only)
- Tests that only assert "a line was logged" without checking the exact bytes for A2 contracts
- Leaving raw `eprintln!`/`println!` in a module after marking it migrated (the CI guard must catch this)
- Coupling this PRD's completion to reactions FEAT-014 (stream-C is decoupled by decision)

---

## Global Acceptance Criteria

Apply to **every** implementation task — task-level `acceptanceCriteria` layer on top.

- Rust: No warnings in `cargo check`
- Rust: No warnings in `cargo clippy -- -D warnings`
- Rust: scoped tests pass with `cargo test -p task-mgr <module>`
- Rust: `cargo fmt --check` passes
- Byte-locked operator-contract messages (`lifecycle_stderr_contract.rs` and any sibling) emit **byte-identical** output after migration — those snapshot tests pass **UNCHANGED**
- No breaking changes to machine-readable stdout (`list`/`show`/`export`/`stats` data stays on stdout, same bytes)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything per-task is returned by `task-mgr next`; everything global is in **this prompt file** (authoritative). If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/logging-standardization.json)
```

Use `$PREFIX` in every CLI call so you stay scoped to this task list. **The same `$PREFIX` is the suffix for `tasks/progress-$PREFIX.txt`, `.task-mgr/logs/task-mgr-$PREFIX.log`, and the grok-stderr capture files** — that prefix-suffix convention is the whole reason logs are per-task-list (concurrent loops / different PRDs never share a log file).

### Commands you'll actually run

| Need | Command |
| --- | --- |
| Pick + claim the next eligible task | `task-mgr next --prefix $PREFIX --claim` |
| Inspect one task | `task-mgr show $PREFIX-TASK-ID` |
| List remaining tasks (debug) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings for a task | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/logging-standardization-prompt.md` | This prompt file (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** (never Read the whole file):

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read on the first iteration (file won't exist). On FEAT-001, **record the CONTRACT-LOG-001 spec** (the boundary contract above) into the progress log under `## CONTRACT-LOG-001` so later iterations have it locally.

---

## Your Task (every iteration)

1. **Resolve prefix and claim**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/logging-standardization.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   If no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** (the `tac | awk | tac` above). Skip on first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do **not** Read `tasks/long-term-learnings.md` / `tasks/learnings.md`. **Do not Read `CLAUDE.md` in full** — the excerpts you need are below; `grep -n -A 10 '<header>' CLAUDE.md` only if a task cites a section not shown.

4. **Verify branch** — `git branch --show-current` matches `feat/logging-standardization`.

5. **Think before coding** — state assumptions; for a migration task, classify each site (A/A2/B) per the decision tree; pick an approach.

6. **Implement** — single task, code + tests in one coherent change.

7. **Run the scoped quality gate** (below — scoped, NOT full suite). For migration tasks this MUST include the byte-lock snapshot tests for any A2 line you touched, plus `cargo test no_raw_prints`.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.

10. **Append progress** — ONE tight block (format below), terminated with `---`.

---

## Behavior Modification Protocol (FEAT-002..006 are `modifiesBehavior: true`)

The "callers/consumers" for these tasks are **operators watching the console** and **any snapshot/pipe test**. Per task:

- Channel A/A2 lines: bytes + FD preserved → no consumer impact (verify with the relevant snapshot/pipe test).
- Channel B lines: move off console (WARN+ default) into the log file → this IS the intended behavior change. Confirm no test asserts that a now-demoted diagnostic appears on the console; if one does, redirect the assertion to the log file or to `TASK_MGR_LOG=debug`.

---

## Quality Checks

### Per-iteration scoped gate (FEAT / FIX tasks)

Pipe to a temp file and grep in the SAME command (project convention):

```bash
cargo fmt --check
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep "^error" /tmp/clippy.txt | head
cargo test -p task-mgr <module_or_fn> 2>&1 | tee /tmp/t.txt | tail -5 && grep -i "FAILED\|error\[" /tmp/t.txt | head
# Migration tasks MUST also run:
cargo test -p task-mgr no_raw_prints 2>&1 | tee -a /tmp/t.txt | tail -3
cargo test lifecycle_stderr_contract 2>&1 | tee -a /tmp/t.txt | tail -3   # if you touched any A2 line
```

Scope from `touchesFiles`. **Do NOT** run the whole workspace suite during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test 2>&1 | tee /tmp/full.txt | tail -10 && grep -i "FAILED\|error\[" /tmp/full.txt | head
```

Fix every failure (including pre-existing). Below ~12 unrelated failures, just fix them; above, spawn a single `FIX-xxx` and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (REVIEW-001 reference)

- `observability::init()` defined but never called from `main.rs` → diagnostics never reach the file.
- `WorkerGuard` from `tracing-appender` dropped at end of `init()` → file writes silently lost. Retain it for process lifetime.
- A migrated module still has a raw `eprintln!` → `no_raw_prints` allow-list not shrunk, or guard not enforcing.
- CLI data line routed to `ui::emit` (stderr) instead of `ui::emit_data` (stdout) → breaks `task-mgr show | grep` pipelines.
- grok sniff buffer accidentally altered in FEAT-006 → `GrokRunner` auth sniff (`runner.rs:918`) + reactions FEAT-014 break.

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| --- | --- | --- | --- |
| REFACTOR-001 | 98 | `REFACTOR-FIX-xxx` (50-97) | Consistent ui::/tracing classification across modules; no per-module reinvention; complexity |
| REVIEW-001 | 99 | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Full suite green; `no_raw_prints` allow-list empty; byte-locked contracts intact; `TASK_MGR_LOG` works; docs |

Spawn follow-ups:

```sh
echo '{"id":"FIX-001","title":"Fix: <issue>","description":"From REVIEW-001: <details>","rootCause":"<file:line>","exactFix":"<change>","verifyCommand":"<cmd>","acceptanceCriteria":["Issue resolved","No new warnings"],"priority":60,"touchesFiles":["affected/file.rs"]}' \
  | task-mgr add --stdin --depended-on-by REVIEW-001
```

Use the **rust-python-code-reviewer** agent when reviewing. If clean, emit `<task-status><REVIEW-ID>:done</task-status>` with a one-line note.

---

## Progress Report Format

APPEND to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [comma-separated paths]
Learnings: [1-3 one-line bullets]
---
```

---

## Stop and Blocked Conditions

**COMPLETE** (after verifying ALL tasks `passes: true`, no new tasks pending, REVIEW-001 passed full suite + empty guard allow-list):

```
<promise>COMPLETE</promise>
```

**BLOCKED** (missing deps / unclear requirements): document in progress file, optionally spawn a clarification task (priority 0), then:

```
<promise>BLOCKED</promise>
```

---

## Reference Code

**Existing primitive to re-home (`src/loop_engine/claude.rs:240`):**

```rust
pub(crate) fn emit_prefixed_lines(slot_label: Option<&str>, text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = write_prefixed_lines(&mut stderr, slot_label, text);
}
// write_prefixed_lines: None → writeln!(text); Some(prefix) → prefix each line;
// empty text with prefix → one prefixed blank line; interior blanks preserved.
// Re-home as ui::emit_prefixed; PRESERVE these exact semantics (a unit test locks them).
```

**Existing `src/output/mod.rs` (extend, don't replace):**

```rust
pub fn warn(msg: &str) { eprintln!("{}", format_warn(msg, should_color())); }
pub fn format_warn(msg: &str, color: bool) -> String { /* [warn] yellow, NO_COLOR-aware */ }
fn should_color() -> bool { /* NO_COLOR + stderr().is_terminal() */ }
```

**Suggested `ui::` surface (FEAT-001):** `ui::emit(msg)` → stderr human progress; `ui::emit_err(msg)` → stderr errors; `ui::emit_data(msg)` → **stdout** machine/CLI data; `ui::emit_prefixed(slot, text)` → re-home of `emit_prefixed_lines`. None add level/timestamp; all honor `should_color`.

**`src/observability.rs` init shape (FEAT-001):**

```rust
// init(active_prefix: Option<&str>) — idempotent, best-effort.
// EnvFilter: env "TASK_MGR_LOG", default "warn" for console layer.
// File layer: tracing_appender daily roll, DEBUG+, to
//   <.task-mgr>/logs/task-mgr-<prefix>.log   (fallback task-mgr.log when prefix is None).
// RETAIN the WorkerGuard for process lifetime (OnceLock / leak) or file writes vanish.
// On file-open failure: console-only, return Ok — NEVER abort.
// Resolve <prefix> via the SAME source progress-<prefix>.txt uses (task-mgr current /
//   TASK_MGR_ACTIVE_PREFIX) — do NOT invent a parallel resolver.
```

---

## Key Learnings (from task-mgr recall)

Authoritative — do NOT Read the learnings files; use `task-mgr recall --query <text>` only for gaps.

- **[4169]** task-mgr logging is a **CLASSIFICATION** problem, not an FD split. Product UX (`emit_prefixed_lines`→stderr), byte-locked operator contracts, and diagnostics all share stderr today. Product → `ui::` (preserve bytes+FD); diagnostics → `tracing`. Mechanical `s/eprintln/tracing/` breaks grep + snapshot tests.
- **[3295]** libtest's default harness intercepts `eprintln!` via thread-local `OUTPUT_CAPTURE`; a `tracing` writer to `io::stderr()` **bypasses** it. Keep tracing console at WARN+ (stays out of normal test output); keep A2 on `ui::` + dup2 snapshot tests.
- **[3416 / 3435 / 3456]** Operator-visible stderr is **byte-locked** by snapshot tests (`lifecycle_stderr_contract.rs` uses `libc::dup2` + `harness=false`; `lifecycle_shadow.rs`). These lines are **A2** — route through `ui::`, never `tracing`, bytes identical.
- **[3855 / 3874]** Log/stderr captures contain non-deterministic timestamps/UUIDs/durations — **normalize** them before asserting in any test that reads the log file back.

---

## CLAUDE.md Excerpts (only what applies here)

- **Test/build output (project §3):** ALWAYS pipe runners to a temp file via `tee` and grep results in the SAME command (one-liners above). Never stream full output.
- **`src/output/mod.rs` already exists** (`warn`/`format_warn`/`should_color`, `NO_COLOR` + TTY discipline) — extend it into `ui::`, don't create a parallel module. Only 3 call sites adopt it today.
- **`emit_prefixed_lines` (`claude.rs:240`)** writes to **stderr**; preserve empty-text + interior-blank-line semantics when re-homing.
- **Overflow recovery (`src/loop_engine/CLAUDE.md`):** the overflow-recovery banner + `check_override_invalidation` escape-valve line are **byte-sensitive operator output** → A2 (`ui::emit`).
- **Parallel-slot (`src/loop_engine/CLAUDE.md`):** halt-threshold abort message, synthetic-deadlock diagnostics, and the slot-0 safety-guard line are **operator-facing** → A2/A, keep on console.
- **`spawn_grok_stderr_sniffer` (`runner.rs:1021`)** returns `(Arc<Mutex<String>>, JoinHandle)`; the buffer is read by `GrokRunner`'s auth sniff (`runner.rs:918`) **and** reactions FEAT-014 — FEAT-006 must leave the buffer byte-for-byte unchanged and only replace the console tee at `runner.rs:1037`.
- **gitignore managed block:** `ensure_progress_gitignore` (`src/commands/init/mod.rs`) manages `tasks/progress-*.txt`; FEAT-001 extends it to cover `.task-mgr/logs/`.
- **Prefix-suffix convention:** progress files are `tasks/progress-<prefix>.txt`; logs mirror this (`task-mgr-<prefix>.log`, `<prefix>-<run>-slotN-iterM-grok-stderr.log`) using the **same** active-prefix resolver — never a parallel one.

---

## Feature-Specific Checks

- **`no_raw_prints` guard:** after migrating a module, remove it from the allow-list in `tests/no_raw_prints.rs`, then `cargo test -p task-mgr no_raw_prints`. The guard must distinguish `eprintln!` from `println!` on a macro boundary (`println!` is a substring of `eprintln!` — a naive grep over-matches).
- **Byte-lock:** if you touched any A2 line, `cargo test lifecycle_stderr_contract` must pass **unchanged**.
- **stdout vs stderr:** for CLI data migrations (FEAT-005), pipe the command and assert the data is on **stdout**, not stderr or the log file.
- **No FEAT-014 coupling:** FEAT-006 must add no `requires[]` and no dependency on the reactions PRD; it only writes the capture file + sets `GROK_TELEMETRY_TRACE_UPLOAD=0` + drops the console tee.

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **feat/logging-standardization**
